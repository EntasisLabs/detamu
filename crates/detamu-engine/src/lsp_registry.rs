//! Persisted language-server registrations for one Detamu database.

use std::path::{Path, PathBuf};

use detamu_language_lsp::LspRegistration;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct RegistryFile {
    registrations: Vec<LspRegistration>,
}

pub struct LspRegistry {
    path: PathBuf,
    file: RegistryFile,
}

impl LspRegistry {
    pub fn path_for(database: &Path) -> PathBuf {
        let mut name = database.file_name().unwrap_or_default().to_os_string();
        name.push(".lsp.json");
        database.with_file_name(name)
    }

    /// # Errors
    ///
    /// Returns an error when the registry file exists but cannot be read or parsed.
    pub fn load(database: &Path) -> Result<Self, String> {
        let path = Self::path_for(database);
        let file = if path.is_file() {
            let text = std::fs::read_to_string(&path)
                .map_err(|error| format!("read lsp registry: {error}"))?;
            serde_json::from_str(&text).map_err(|error| format!("parse lsp registry: {error}"))?
        } else {
            RegistryFile::default()
        };
        Ok(Self { path, file })
    }

    pub fn registrations(&self) -> &[LspRegistration] {
        &self.file.registrations
    }

    /// # Errors
    ///
    /// Returns an error when the registration is invalid or the file cannot be written.
    pub fn upsert(&mut self, registration: LspRegistration) -> Result<LspRegistration, String> {
        let registration = registration.prepare()?;
        if let Some(existing) = self
            .file
            .registrations
            .iter_mut()
            .find(|item| item.id == registration.id)
        {
            *existing = registration.clone();
        } else {
            self.file.registrations.push(registration.clone());
        }
        self.file
            .registrations
            .sort_by(|left, right| left.id.cmp(&right.id));
        self.save()?;
        Ok(registration)
    }

    /// # Errors
    ///
    /// Returns an error when the registry file cannot be rewritten.
    pub fn remove(&mut self, id: &str) -> Result<bool, String> {
        let before = self.file.registrations.len();
        self.file.registrations.retain(|item| item.id != id);
        let removed = self.file.registrations.len() != before;
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    fn save(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("create lsp registry directory: {error}"))?;
        }
        let text = serde_json::to_string_pretty(&self.file)
            .map_err(|error| format!("encode lsp registry: {error}"))?;
        let temporary = self.path.with_extension("json.tmp");
        std::fs::write(&temporary, text).map_err(|error| format!("write lsp registry: {error}"))?;
        std::fs::rename(&temporary, &self.path)
            .map_err(|error| format!("replace lsp registry: {error}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_round_trip_replaces_the_same_id() {
        let directory =
            std::env::temp_dir().join(format!("detamu-lsp-registry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("temp dir");
        let database = directory.join("detamu.surrealkv");
        let mut registry = LspRegistry::load(&database).expect("load missing registry");
        registry.upsert(sample("python")).expect("insert");
        registry
            .upsert(LspRegistration {
                command: "basedpyright-langserver".to_owned(),
                ..sample("python")
            })
            .expect("replace");
        let loaded = LspRegistry::load(&database).expect("reload");
        assert_eq!(loaded.registrations().len(), 1);
        assert_eq!(loaded.registrations()[0].command, "basedpyright-langserver");
        assert!(registry.remove("python").expect("remove"));
        assert_eq!(
            LspRegistry::load(&database).expect("empty").registrations(),
            []
        );
        let _ = std::fs::remove_dir_all(&directory);
    }

    fn sample(id: &str) -> LspRegistration {
        LspRegistration {
            id: id.to_owned(),
            language: "python".to_owned(),
            command: "pyright-langserver".to_owned(),
            arguments: vec!["--stdio".to_owned()],
            extensions: vec!["py".to_owned()],
            initialization_options: None,
        }
    }
}
