use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../../adapters/channels/web/"]
pub struct Assets;
