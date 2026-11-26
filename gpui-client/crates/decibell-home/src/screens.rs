//! The people screens, drawn from [`crate::fixture`] (no network, no clock): the whole window
//! (the shell signed in as perf1) in each scene. `decibell-capture --screen <name>`, `decibell
//! --screen <name>`.

use decibell_harness::Screen;
use decibell_shell::Shell;
use decibell_ui::AssumeClientDecorations;
use gpui::{AnyView, App, AppContext as _, Window};

use crate::fixture::{self, Scene};

fn build(scene: Scene, cx: &mut App) -> AnyView {
    // Captures render one frame on a clock that doesn't run: entrances show their end.
    if cx.has_global::<AssumeClientDecorations>() {
        cx.set_reduce_motion(true);
    }
    fixture::apply(scene, cx);
    // The capture tool skips the app's boot: register what the screen draws (once).
    crate::init(cx);
    cx.new(Shell::new).into()
}

macro_rules! screens {
    ($($scene:ident),* $(,)?) => {
        vec![$(Screen {
            name: Scene::$scene.name(),
            about: Scene::$scene.about(),
            build: |_: &mut Window, cx: &mut App| build(Scene::$scene, cx),
        }),*]
    };
}

pub fn screens() -> Vec<Screen> {
    screens![
        FriendsOnline,
        FriendsAll,
        FriendsPending,
        FriendsBlocked,
        FriendsAdd,
        DmPlain,
        DmEncrypted,
        DmLocked,
        DmKeyChanged,
        DmWithRail,
        ProfilePopup,
        ProfileModal,
        ProfilePrivacy,
        CustomStatus,
        UserMenuOwn,
        UserMenuMember,
        UserMenuModerator,
        NicknameModal,
        BanModal,
    ]
}
