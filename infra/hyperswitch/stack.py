#!/usr/bin/env python3
"""Prepare and operate only the named, disposable pinned Hyperswitch sandbox."""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import subprocess
import tarfile
import tempfile
import urllib.parse
import urllib.request

HERE = Path(__file__).resolve().parent
RUNTIME = HERE / '.runtime'
LOCK = json.loads((HERE / 'versions.json').read_text())


def run(args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def private_file(path, content):
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix=path.name + '.', dir=path.parent)
    try:
        with os.fdopen(descriptor, 'w') as file:
            file.write(content)
            file.flush()
            os.fsync(file.fileno())
        os.replace(temporary, path)
        directory = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def source():
    repo = HERE / '.repo'
    if not repo.exists():
        run(['git', 'clone', '--depth', '1', '--branch', LOCK['source']['tag'], LOCK['source']['repository'], str(repo)])
    actual = subprocess.check_output(['git', '-C', str(repo), 'rev-parse', 'HEAD'], text=True).strip()
    if actual != LOCK['source']['commit']:
        raise SystemExit('Source commit differs from versions.json; use a separate checkout, never reset unrelated edits.')
    if subprocess.check_output(['git', '-C', str(repo), 'status', '--porcelain'], text=True).strip():
        raise SystemExit('Pinned source has edits; preserve them and restore a clean separate source checkout.')
    return repo


def replace_value(text, section, key, value):
    pattern = rf'(?ms)(^\[{re.escape(section)}\]\s*\n)(.*?)(?=^\[|\Z)'
    found = re.search(pattern, text)
    if not found:
        raise ValueError(f'Missing upstream section {section}')
    body = found.group(2)
    line = f'{key} = {json.dumps(value)}' if not isinstance(value, bool) else f'{key} = {str(value).lower()}'
    key_pattern = rf'(?m)^{re.escape(key)}\s*=.*$'
    if re.search(key_pattern, body):
        body = re.sub(key_pattern, lambda _: line, body, count=1)
    else:
        body += line + '\n'
    return text[:found.start(2)] + body + text[found.end(2):]


def public_origin(value):
    parsed = urllib.parse.urlsplit(value)
    if parsed.scheme != 'https' or not parsed.hostname or parsed.username or parsed.password or parsed.path not in ('', '/') or parsed.query or parsed.fragment:
        raise ValueError('Public sandbox origin must be a bare HTTPS origin without credentials/query/fragment.')
    return value.rstrip('/')


def render(config, repo):
    text = (repo / 'config/docker_compose.toml').read_text()
    replacements = [
        ('log.console', 'level', 'WARN'), ('log.telemetry', 'metrics_enabled', False),
        ('redis', 'host', 'redis'),
        ('secrets', 'admin_api_key', config['adminKey']), ('secrets', 'jwt_secret', config['jwtSecret']),
        ('secrets', 'master_enc_key', config['masterKey']), ('api_keys', 'hash_key', config['apiHashKey']),
        ('user_auth_methods', 'encryption_key', config['userEncryptionKey']),
        ('user', 'base_url', config['controlUrl']),
        ('email.smtp', 'host', 'mailpit'),
        ('multitenancy.tenants.public', 'base_url', config['routerUrl']),
        ('multitenancy.tenants.public.user', 'control_center_url', config['controlUrl']),
        # Upstream officially supports this versioned file source. This sandbox
        # uses its shipped policy snapshot, not an unpersisted demo policy server.
        ('superposition', 'endpoint', 'http://127.0.0.1:1'),
        ('superposition', 'request_timeout', 2),
        ('superposition', 'backup_file_path', '/local/config/superposition_seed.toml'),
        ('generic_link.payment_method_collect', 'sdk_url', config['sdkUrl'] + '/HyperLoader.js'),
        ('generic_link.payout_link', 'sdk_url', config['sdkUrl'] + '/HyperLoader.js'),
    ]
    for section in ['master_database', 'replica_database', 'analytics_database', 'accounts_database']:
        if f'[{section}]' in text:
            replacements.append((section, 'password', config['dbPassword']))
    # Some upstream bundles carry an additional database connection.
    for section in re.findall(r'^\[([^\]]+)\]', text, re.M):
        if section.endswith('database') and (section, 'password', config['dbPassword']) not in replacements:
            replacements.append((section, 'password', config['dbPassword']))
    for section, key, value in replacements:
        text = replace_value(text, section, key, value)
    private_file(RUNTIME / 'config/docker_compose.toml', text)
    private_file(RUNTIME / 'config/superposition_seed.toml', (repo / 'config/superposition_seed.toml').read_text())
    dashboard = (repo / 'config/dashboard.toml').read_text()
    dashboard = replace_value(dashboard, 'default.endpoints', 'api_url', config['routerUrl'])
    dashboard = replace_value(dashboard, 'default.endpoints', 'sdk_url', config['sdkUrl'] + '/HyperLoader.js')
    private_file(RUNTIME / 'config/dashboard.toml', dashboard)
    private_file(RUNTIME / 'config/dashboard_theme.json', (repo / 'config/dashboard_theme.json').read_text())
    images = LOCK['images']
    values = {
        'HS_ROUTER_IMAGE': images['router'], 'HS_POSTGRES_IMAGE': images['postgres'], 'HS_REDIS_IMAGE': images['redis'],
        'HS_SDK_IMAGE': images['sdk'], 'HS_CONTROL_CENTER_IMAGE': images['controlCenter'], 'HS_MAILPIT_IMAGE': images['mailpit'],
        'HS_DB_PASSWORD': config['dbPassword'], 'HS_PUBLIC_ROUTER_URL': config['routerUrl'],
        'HS_PUBLIC_SDK_URL': config['sdkUrl'],
    }
    values.update({f'HS_{name}_PORT': str(port) for name, port in config['ports'].items()})
    private_file(RUNTIME / 'stack.env', ''.join(f'{key}={value}\n' for key, value in values.items()))


def prepare(args):
    RUNTIME.mkdir(mode=0o700, exist_ok=True)
    RUNTIME.chmod(0o700)
    with (RUNTIME / 'prepare.lock').open('w') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        prepare_locked(args)


def prepare_locked(args):
    repo = source()
    path = RUNTIME / 'private.json'
    if path.exists():
        config = json.loads(path.read_text())
        if config['sourceSha'] != LOCK['source']['commit']:
            raise SystemExit('Existing sandbox belongs to another source version; use its documented upgrade procedure.')
    else:
        config = {
            'project': 'oxsum-hs-' + secrets.token_hex(4), 'sourceSha': LOCK['source']['commit'],
            'dbPassword': secrets.token_hex(24), 'adminKey': secrets.token_hex(32), 'jwtSecret': secrets.token_hex(32),
            'masterKey': secrets.token_hex(32), 'apiHashKey': secrets.token_hex(32), 'userEncryptionKey': secrets.token_hex(32),
            'ports': {'ROUTER': 18080, 'SDK': 19050, 'CONTROL': 19000, 'SMTP': 1025, 'MAIL_UI': 8025},
            'routerUrl': 'http://127.0.0.1:18080', 'sdkUrl': 'http://127.0.0.1:19050', 'controlUrl': 'http://127.0.0.1:19000',
        }
    if args.router_origin or args.sdk_origin:
        if not args.router_origin or not args.sdk_origin:
            raise SystemExit('Supply both HTTPS --router-origin and --sdk-origin together.')
        config['routerUrl'] = public_origin(args.router_origin)
        config['sdkUrl'] = public_origin(args.sdk_origin)
    private_file(path, json.dumps(config, indent=2) + '\n')
    arch = subprocess.check_output(['docker', 'info', '--format', '{{.Architecture}}'], text=True).strip()
    arch = {'aarch64': 'arm64', 'x86_64': 'amd64'}.get(arch, arch)
    artifact = LOCK['diesel'].get(arch)
    if not artifact:
        raise SystemExit('Only Docker linux/arm64 and linux/amd64 are supported by the pinned migration tool.')
    tools = RUNTIME / 'tools'; tools.mkdir(exist_ok=True)
    filename = f'diesel_cli-{artifact["target"]}.tar.xz'
    archive = tools / filename
    if not archive.exists():
        url = f'https://github.com/diesel-rs/diesel/releases/download/v{LOCK["diesel"]["version"]}/{filename}'
        with urllib.request.urlopen(url, timeout=60) as response:
            archive.write_bytes(response.read())
    if hashlib.sha256(archive.read_bytes()).hexdigest() != artifact['sha256']:
        raise SystemExit('Migration tool checksum mismatch; archive was preserved for inspection.')
    with tarfile.open(archive) as tar:
        member = tar.getmember(f'diesel_cli-{artifact["target"]}/diesel')
        if not member.isfile():
            raise SystemExit('Migration tool archive does not contain the expected regular binary.')
        with tar.extractfile(member) as content, (tools / 'diesel').open('wb') as target:
            shutil.copyfileobj(content, target)
    (tools / 'diesel').chmod(0o755)
    # Runtime migrations need no schema-codegen patch or mutable upstream file.
    private_file(tools / 'diesel.toml', '# Intentionally no print_schema section.\n')
    render(config, repo)
    print(f'Prepared {config["project"]}; secrets retained in .runtime/private.json (0600).')


def compose(arguments, capture=False):
    config = json.loads((RUNTIME / 'private.json').read_text())
    if not re.fullmatch(r'oxsum-hs-[a-f0-9]{8}', config['project']):
        raise SystemExit('Refusing an unrecognized sandbox project name.')
    args = ['docker', 'compose', '--project-name', config['project'], '--env-file', str(RUNTIME / 'stack.env'), '-f', str(HERE / 'compose.yml'), *arguments]
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith(('HS_', 'COMPOSE_'))}
    return run(args, capture_output=capture, text=True, env=environment)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['prepare', 'check', 'up', 'stop', 'status'])
    parser.add_argument('--router-origin')
    parser.add_argument('--sdk-origin')
    args = parser.parse_args()
    if args.command == 'prepare':
        prepare(args)
    elif args.command == 'check':
        compose(['config', '--quiet'])
        print('Compose configuration valid; no services started.')
    elif args.command == 'up':
        # Config files are directory/file mounts; changed contents alone do not
        # cause Compose to recreate a process. The sandbox keeps named volumes.
        compose(['up', '-d', '--force-recreate', '--wait', '--wait-timeout', '240'])
    elif args.command == 'stop':
        compose(['stop'])
    else:
        compose(['ps'])


if __name__ == '__main__':
    main()
