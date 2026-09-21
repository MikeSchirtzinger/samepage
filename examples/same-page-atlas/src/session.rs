//! Local discovery for terminal agents. Credentials stay in a private file;
//! the manifest identifies the page without disclosing its token.
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

pub struct Session {
    manifest: PathBuf,
    credential: PathBuf,
}
impl Session {
    pub fn publish(
        root: &Path,
        address: &str,
        token: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let directory = root.join(".samepage/sessions");
        fs::create_dir_all(&directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
        }
        let pid = std::process::id();
        let credential = directory.join(format!("{pid}.token"));
        let manifest = directory.join(format!("{pid}.json"));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(&credential)?.write_all(token.as_bytes())?;
        let session = Self {
            manifest,
            credential,
        };
        let url = format!("http://{}", address.replacen("0.0.0.0", "127.0.0.1", 1));
        let resources = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()?;
        let data = serde_json::json!({"schema_version":1,"kind":"same-page-understanding","pid":pid,"repository":root.canonicalize()?,"url":url,"credential_file":session.credential.canonicalize()?,"entry_tool":"atlas_session","wait_tool":"await_input","agent_guide":resources.join("docs/agent-harness/same-page.md"),"terminal_client":resources.join("dev/samepage")});
        options
            .open(&session.manifest)?
            .write_all(&serde_json::to_vec_pretty(&data)?)?;
        Ok(session)
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.manifest);
        let _ = fs::remove_file(&self.credential);
    }
}
