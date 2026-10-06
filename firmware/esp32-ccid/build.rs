fn main() {
    embuild::espidf::sysenv::output();

    // WiFi credentials are baked via option_env! in the crate sources —
    // cargo must treat them as compilation inputs. Without this, changing
    // NUCULA_WIFI_SSID/PASS and running `cargo build` silently reuses the
    // cached rlib with the OLD credentials (the bench flash stale-image
    // bug: three consecutive "successful" flashes wrote the wrong SSID).
    println!("cargo:rerun-if-env-changed=NUCULA_WIFI_SSID");
    println!("cargo:rerun-if-env-changed=NUCULA_WIFI_PASS");
}
