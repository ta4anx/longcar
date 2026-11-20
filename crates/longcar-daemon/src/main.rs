// SPDX-License-Identifier: GPL-3.0-or-later

//! Daemon entry point.

fn main() -> std::process::ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("longcar-daemon: cannot start async runtime: {err}");
            return std::process::ExitCode::FAILURE;
        }
    };
    runtime.block_on(longcar_daemon::run())
}
