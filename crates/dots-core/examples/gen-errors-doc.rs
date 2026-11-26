//! Regenerates `docs/errors.md` and `schemas/*.schema.json`.
//! Run: `cargo run -p dots-core --example gen-errors-doc`

use std::path::Path;

fn main() -> std::io::Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::write(root.join("docs/errors.md"), dots_core::errors::render_doc())?;
    std::fs::create_dir_all(root.join("schemas"))?;
    for c in dots_core::commands::schema_commands() {
        let path = root
            .join("schemas")
            .join(dots_core::commands::schema_file_name(c));
        std::fs::write(path, dots_core::commands::schema_text(c))?;
    }
    println!("regenerated docs/errors.md and schemas/");
    Ok(())
}
