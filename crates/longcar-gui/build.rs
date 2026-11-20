// SPDX-License-Identifier: GPL-3.0-or-later

fn main() {
    glib_build_tools::compile_resources(
        &["../../data"],
        "../../data/longcar.gresource.xml",
        "longcar.gresource",
    );
}
