use {super::*, settings::Settings};

#[derive(Debug, Parser)]
pub struct SettingsCmd;

impl SettingsCmd {
    pub async fn run(self, settings: Settings) -> Result {
        println!("{}", serde_json::to_string_pretty(&settings)?);
        Ok(())
    }
}
