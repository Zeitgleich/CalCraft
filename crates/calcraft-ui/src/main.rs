//! Entry point of the web interface. `wasm-bindgen` runs `main` when the
//! module is initialised. Built by `scripts/build-ui.sh`; on other targets
//! the binary is empty.

#[cfg(target_arch = "wasm32")]
mod app;

#[cfg(target_arch = "wasm32")]
fn main() {
    app::start();
}

#[cfg(not(target_arch = "wasm32"))]
fn main() {}
