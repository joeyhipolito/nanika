use std::sync::Arc;

use async_trait::async_trait;
use dust_sdk::{
    ActionResult, Capability, Component, DustPlugin, ListItem, PluginManifest,
};

struct HelloPlugin;

#[async_trait]
impl DustPlugin for HelloPlugin {
    fn plugin_id(&self) -> &str {
        "hello-plugin"
    }

    async fn manifest(&self) -> PluginManifest {
        PluginManifest {
            name: "Hello Plugin".into(),
            version: "0.1.0".into(),
            description: "Greets the world. Keywords: hello, greet.".into(),
            capabilities: vec![Capability::Command {
                prefix: "hello".into(),
            }],
            icon: None,
        }
    }

    async fn render(&self) -> Vec<Component> {
        vec![Component::List {
            title: Some("Greetings".into()),
            items: vec![
                ListItem::new("hello-world", "Hello, World!"),
                ListItem::new("hello-nanika", "Hello, Nanika!"),
                ListItem::new("hello-dust", "Hello, Dust!"),
            ],
        }]
    }

    async fn action(&self, _params: serde_json::Value) -> ActionResult {
        ActionResult::ok()
    }
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    dust_sdk::run(Arc::new(HelloPlugin)).await
}
