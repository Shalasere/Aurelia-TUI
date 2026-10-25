//! HLTB action discovery uses the same browser model as the other menu entries.
use aurelia_tui::browse::{Action, Browser};
use aurelia_tui::interface::{aurelia::LibraryGameJson, game::Game};

fn isolate_config() {
    // A separate test binary avoids races with other suites' config overrides.
    let dir = std::env::temp_dir().join(format!("aurelia-hltb-menu-{}", std::process::id()));
    unsafe {
        std::env::set_var("AURELIA_TUI_DIR", dir);
    }
}

fn game(id: u32, name: &str, installed: bool) -> Game {
    Game::from_library(LibraryGameJson {
        app_id: id,
        name: name.into(),
        is_installed: installed,
        install_path: None,
        update_available: false,
        is_owned: true,
        is_family_shared: false,
        platform: None,
        active_branch: None,
        assets: None,
        store_url: None,
    })
}

#[test]
fn hltb_page_is_discoverable_without_requiring_an_install() {
    isolate_config();
    for installed in [true, false] {
        let mut browser = Browser::new(vec![game(400, "Portal", installed)]);
        browser.open_actions();
        for character in "howlongtobeat".chars() {
            browser.actions_filter_push(character);
        }
        assert_eq!(browser.selected_action(), Some(Action::Hltb));
        assert_eq!(browser.filtered_actions()[0].key, "t");
        browser.open_hltb();
        assert!(browser.notice.as_deref().unwrap().contains("waiting"));
    }
}
