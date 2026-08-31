use serde::{Deserialize, Serialize};

pub const PLAYER_READ: &str = "player:read";
pub const PLAYER_CONTROL: &str = "player:control";
pub const LIBRARY_READ: &str = "library:read";
pub const AUDIO_DEVICES: &str = "audio:devices";
pub const AUDIO_OUTPUT: &str = "audio:output";
pub const HTTP_LISTEN: &str = "http:listen";
pub const FS_PLUGIN_DIR: &str = "fs:plugin-dir";

pub const ALL_PERMISSIONS: &[&str] = &[
    PLAYER_READ,
    PLAYER_CONTROL,
    LIBRARY_READ,
    AUDIO_DEVICES,
    AUDIO_OUTPUT,
    HTTP_LISTEN,
    FS_PLUGIN_DIR,
];

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_main")]
    pub main: String,
    /// `"js"` or `"native"`. If omitted, taken from `main` (`.dll` → native).
    #[serde(default)]
    pub runtime: Option<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub settings: Vec<PluginSettingSpec>,
    #[serde(default)]
    pub enabled_by_default: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingKind {
    Number,
    Boolean,
    String,
}

impl Default for SettingKind {
    fn default() -> Self {
        Self::String
    }
}

/// One field the host can render in Settings without a custom UI page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginSettingSpec {
    pub key: String,
    #[serde(rename = "type")]
    pub kind: SettingKind,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub default: Option<serde_json::Value>,
}

impl PluginSettingSpec {
    pub fn sanitize(&self, value: Option<&serde_json::Value>) -> serde_json::Value {
        match self.kind {
            SettingKind::Number => {
                let mut n = value
                    .and_then(serde_json::Value::as_f64)
                    .or_else(|| self.default.as_ref().and_then(serde_json::Value::as_f64))
                    .unwrap_or(0.0);
                if let Some(min) = self.min {
                    n = n.max(min);
                }
                if let Some(max) = self.max {
                    n = n.min(max);
                }
                if n.is_finite() && n.fract() == 0.0 {
                    serde_json::json!(n as i64)
                } else {
                    serde_json::json!(n)
                }
            }
            SettingKind::Boolean => serde_json::json!(value
                .and_then(serde_json::Value::as_bool)
                .or_else(|| self.default.as_ref().and_then(serde_json::Value::as_bool))
                .unwrap_or(false)),
            SettingKind::String => serde_json::json!(value
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .or_else(|| self
                    .default
                    .as_ref()
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string))
                .unwrap_or_default()),
        }
    }
}

impl PluginManifest {
    pub fn resolved_config(&self, stored: &serde_json::Value) -> serde_json::Value {
        let mut out = match stored.as_object() {
            Some(obj) => obj.clone(),
            None => serde_json::Map::new(),
        };
        for spec in &self.settings {
            let current = out.get(&spec.key).cloned();
            out.insert(spec.key.clone(), spec.sanitize(current.as_ref()));
        }
        serde_json::Value::Object(out)
    }
}

fn default_main() -> String {
    "index.js".into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginRuntime {
    Js,
    Native,
}

impl PluginManifest {
    pub fn runtime(&self) -> PluginRuntime {
        match self.runtime.as_deref().map(|s| s.trim().to_ascii_lowercase()) {
            Some(s) if s == "native" || s == "dll" => PluginRuntime::Native,
            Some(s) if s == "js" || s == "javascript" => PluginRuntime::Js,
            _ => {
                let ext = std::path::Path::new(&self.main)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if ext == "dll" {
                    PluginRuntime::Native
                } else {
                    PluginRuntime::Js
                }
            }
        }
    }
}

impl PluginManifest {
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
        let manifest: Self = serde_json::from_str(&raw)
            .map_err(|e| format!("Invalid plugin.json ({}): {e}", path.display()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), String> {
        if !is_valid_plugin_id(&self.id) {
            return Err(format!(
                "Invalid plugin id '{}'. Use something like 'user.mixpamp'.",
                self.id
            ));
        }
        if self.name.trim().is_empty() {
            return Err("Plugin name is empty".into());
        }
        if let Some(runtime) = &self.runtime {
            let r = runtime.trim().to_ascii_lowercase();
            if !matches!(r.as_str(), "js" | "javascript" | "native" | "dll") {
                return Err(format!(
                    "Unknown runtime '{runtime}' in {}. Use \"js\" or \"native\".",
                    self.id
                ));
            }
        }
        for perm in &self.permissions {
            if !ALL_PERMISSIONS.contains(&perm.as_str()) {
                return Err(format!("Unknown permission '{perm}' in {}", self.id));
            }
        }
        let mut seen = std::collections::HashSet::new();
        for spec in &self.settings {
            if spec.key.trim().is_empty() {
                return Err(format!("Empty settings key in {}", self.id));
            }
            if !seen.insert(spec.key.clone()) {
                return Err(format!("Duplicate settings key '{}' in {}", spec.key, self.id));
            }
        }
        Ok(())
    }
}

pub fn is_valid_plugin_id(id: &str) -> bool {
    let mut parts = id.split('.');
    let Some(first) = parts.next() else {
        return false;
    };
    let Some(second) = parts.next() else {
        return false;
    };
    if first.is_empty() || second.is_empty() {
        return false;
    }
    id.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_' || c == '-')
        && !id.starts_with('.')
        && !id.ends_with('.')
        && !id.contains("..")
}
