//! Render one sample bill per variant (fixed keys), the design kit, and one
//! custom-template sample per variant for visual inspection:
//! `cargo run -p wallet-core --example render_samples -- <out-dir>`

use wallet_core::{bill, template, Variant};

fn main() {
    let out_dir = std::env::args().nth(1).unwrap_or_else(|| ".".to_string());
    let mut k1 = [0u8; 32];
    k1[31] = 1;
    let kb = [0x11u8; 32];
    let ts = "2026-07-02 12:00:00 UTC";

    let kit = template::render_design_kit().unwrap();
    std::fs::write(format!("{out_dir}/design_kit_template.png"), &kit.template_png).unwrap();
    std::fs::write(format!("{out_dir}/design_kit_satoshi.png"), &kit.satoshi_example_png).unwrap();
    println!("{out_dir}/design_kit_template.png + design_kit_satoshi.png");

    for (name, variant, backup) in [
        ("segwit", Variant::Segwit, None),
        ("taproot", Variant::Taproot, None),
        ("taproot_tweaked", Variant::TaprootBackup, Some((kb, 0))),
    ] {
        let wallet = wallet_core::from_privkeys(variant, &k1, backup).unwrap();
        let png = bill::compose_bill(&wallet, "2026", ts).unwrap();
        let path = format!("{out_dir}/sample_{name}.png");
        std::fs::write(&path, png).unwrap();

        let custom = template::compose_custom_bill(&kit.satoshi_example_png, &wallet, ts).unwrap();
        let custom_path = format!("{out_dir}/sample_custom_{name}.png");
        std::fs::write(&custom_path, custom).unwrap();
        println!("{path} + sample_custom_{name}.png  addr={}", wallet.address);

        // The other shipped designs, through the same custom-template path a
        // user's own PNG takes.
        for (design, png) in [
            ("cypherpunk", &include_bytes!("../assets/templates/cypherpunk.png")[..]),
            ("ledger", &include_bytes!("../assets/templates/ledger.png")[..]),
        ] {
            let bill = template::compose_custom_bill(png, &wallet, ts).unwrap();
            std::fs::write(format!("{out_dir}/sample_{design}_{name}.png"), bill).unwrap();
        }
    }
}
