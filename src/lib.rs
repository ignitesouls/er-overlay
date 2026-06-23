pub mod er;
pub mod overlay;
pub mod util;

use hudhook::{Hudhook, eject, hooks::dx12::ImguiDx12Hooks, tracing::error};
use overlay::ui::EROverlayUi;
use std::thread;

#[unsafe(no_mangle)]
#[allow(non_snake_case)]
pub unsafe extern "C" fn DllMain(_hmodule: u64, reason: u32) -> bool {
    const DLL_PROCESS_ATTACH: u32 = 1;

    if reason == DLL_PROCESS_ATTACH {
        let _ = thread::spawn(|| {
            if let Err(e) = Hudhook::builder()
                .with::<ImguiDx12Hooks>(EROverlayUi::new())
                .build()
                .apply()
            {
                error!("Couldn't apply hooks: {e:?}");
                eject();
            }
        });
    }
    true
}
