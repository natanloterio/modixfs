use crate::manifest::{FileKind, Manifest};

pub fn generate_how_to(manifest: &Manifest) -> String {
    let mut out = String::new();
    let name = manifest.name.as_deref().unwrap_or("this tool");
    out.push_str(&format!("# {}\n\n", name));
    if let Some(desc) = &manifest.description {
        out.push_str(&format!("{}\n\n", desc));
    }
    if !manifest.files.is_empty() {
        out.push_str("## Files\n\n");
        for spec in &manifest.files {
            let kind_str = match spec.kind {
                FileKind::WriteInvoke => "write_invoke",
                FileKind::ReadInvoke => "read_invoke",
                FileKind::Passthrough => "passthrough",
                FileKind::Readonly => "readonly",
            };
            out.push_str(&format!("- **{}** (`{}`)", spec.name, kind_str));
            if let Some(h) = &spec.handler {
                out.push_str(&format!(" — handler: `{}`", h));
            }
            out.push('\n');
        }
        out.push('\n');
    }
    if !manifest.env.is_empty() {
        out.push_str("## Required secrets\n\n");
        for e in &manifest.env {
            out.push_str(&format!("- `{}`", e.name));
            if let Some(desc) = &e.description {
                out.push_str(&format!(" — {}", desc));
            }
            if e.required {
                out.push_str(" *(required)*");
            }
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{EnvDecl, FileSpec, Manifest};

    fn make_manifest(name: &str, desc: Option<&str>, files: Vec<FileSpec>, env: Vec<EnvDecl>) -> Manifest {
        Manifest {
            name: Some(name.into()),
            description: desc.map(|s| s.into()),
            version: None,
            env,
            files,
        }
    }

    #[test]
    fn generate_includes_name_and_description() {
        let m = make_manifest("weather", Some("Get the weather forecast."), vec![], vec![]);
        let out = generate_how_to(&m);
        assert!(out.contains("weather"));
        assert!(out.contains("Get the weather forecast."));
    }

    #[test]
    fn generate_lists_file_specs() {
        let m = make_manifest("demo", None, vec![
            FileSpec { name: "forecast".into(), kind: FileKind::ReadInvoke, handler: Some("date".into()) },
            FileSpec { name: "notes.txt".into(), kind: FileKind::Passthrough, handler: None },
        ], vec![]);
        let out = generate_how_to(&m);
        assert!(out.contains("forecast"));
        assert!(out.contains("read_invoke"));
        assert!(out.contains("notes.txt"));
        assert!(out.contains("passthrough"));
    }
}
