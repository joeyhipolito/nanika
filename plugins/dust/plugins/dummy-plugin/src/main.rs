use std::sync::Arc;

use async_trait::async_trait;
use dust_sdk::{
    ActionResult, Capability, Component, DustPlugin, ListItem, PluginManifest,
};

struct DummyPlugin;

#[async_trait]
impl DustPlugin for DummyPlugin {
    fn plugin_id(&self) -> &str {
        "dummy-plugin"
    }

    async fn manifest(&self) -> PluginManifest {
        PluginManifest {
            name: "Dummy Plugin".into(),
            version: "0.1.0".into(),
            description: "Hot-plug test. Keywords: dummy, test, hotplug.".into(),
            capabilities: vec![Capability::Command {
                prefix: "dummy".into(),
            }],
            icon: None,
        }
    }

    async fn render(&self) -> Vec<Component> {
        vec![Component::List {
            title: Some("Dummy Items".into()),
            items: vec![
                ListItem::new("dummy-1", "I was hot-plugged!"),
                ListItem::new("dummy-2", "No restart needed"),
                ListItem::new("dummy-3", "Dust is alive"),
            ],
        }]
    }

    async fn action(&self, _params: serde_json::Value) -> ActionResult {
        ActionResult::ok()
    }
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    dust_sdk::run(Arc::new(DummyPlugin)).await
}
