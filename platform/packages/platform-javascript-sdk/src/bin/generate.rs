use std::{fs, path::PathBuf};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let sites = root.join("../../apps/sites-service/src");
    let declarations = platform_javascript_sdk::declarations();
    let backend = format!(
        "{}\n{}\n{}",
        fs::read_to_string(sites.join("sdk.base.d.ts"))?,
        declarations,
        fs::read_to_string(sites.join("sdk.compat.d.ts"))?
    );
    let files = [
        (
            root.join("sites.d.ts"),
            platform_javascript_sdk::sites_declarations(),
        ),
        (root.join("platform.d.ts"), declarations),
        (
            root.join("METHODS.md"),
            platform_javascript_sdk::documentation(),
        ),
        (sites.join("sdk.d.ts"), backend),
    ];
    let check = std::env::args().any(|a| a == "--check");
    for (path, contents) in files {
        if check {
            if fs::read_to_string(&path)? != contents {
                return Err(format!("Generated SDK is stale: {}", path.display()).into());
            }
        } else {
            fs::write(path, contents)?;
        }
    }
    Ok(())
}
