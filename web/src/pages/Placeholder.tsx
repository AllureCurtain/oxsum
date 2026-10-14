// Pages not yet ported — the parity checklist in web/README.md tracks them.
// Each links back to the authoritative Topcoat page until the port lands.
export function Placeholder(props: { title: string; topcoat: string }) {
  return (
    <div className="flex flex-col gap-4">
      <h1 className="text-2xl font-bold">{props.title}</h1>
      <p className="text-muted">
        Not yet ported to the React dashboard — the Topcoat page at{" "}
        <a className="text-primary" href={props.topcoat}>
          {props.topcoat}
        </a>{" "}
        stays authoritative (issue #234 parity checklist).
      </p>
    </div>
  );
}
