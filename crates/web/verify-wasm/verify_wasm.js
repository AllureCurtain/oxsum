// Placeholder for the wasm-bindgen output `scripts/build-verify-wasm.sh`
// emits (committed so `cargo build` works without the wasm toolchain — the
// web/dist pattern). The /verify page's loader imports this module's default
// export; the throw makes the import fail, and the page quietly keeps its
// server-side form. A real build overwrites this file in the image.
export default async function init() {
  throw new Error("the verify wasm bundle was not built");
}
