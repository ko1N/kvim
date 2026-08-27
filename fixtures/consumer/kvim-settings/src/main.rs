use kvim_settings::EditorSettings;

fn main() {
    EditorSettings::default()
        .realize()
        .expect("default settings are valid");
}
