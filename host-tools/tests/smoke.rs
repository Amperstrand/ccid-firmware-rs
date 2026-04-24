use pcsc::{Context, Error, Protocols, Scope, ShareMode, MAX_BUFFER_SIZE};

fn get_readers(ctx: &Context) -> Vec<std::ffi::CString> {
    match ctx.list_readers_owned() {
        Ok(readers) => readers,
        Err(Error::NoReadersAvailable) => Vec::new(),
        Err(e) => {
            eprintln!("pcsc list_readers error: {e}");
            Vec::new()
        }
    }
}

#[test]
fn test_list_readers() {
    let result = ccid_host_tools::list_readers();
    match result {
        Ok(readers) => {
            println!("found {} reader(s)", readers.len());
            for r in &readers {
                println!("  - {r}");
            }
        }
        Err(e) => {
            println!("list_readers returned error (expected in CI): {e}");
        }
    }
}

#[test]
fn test_connect_first_reader() {
    let ctx = match Context::establish(Scope::User) {
        Ok(ctx) => ctx,
        Err(e) => {
            println!("SKIP: cannot establish PC/SC context: {e}");
            return;
        }
    };

    let readers = get_readers(&ctx);
    if readers.is_empty() {
        println!("SKIP: no PC/SC readers connected");
        return;
    }

    let reader = &readers[0];
    println!("connecting to reader: {:?}", reader);

    match ctx.connect(reader, ShareMode::Shared, Protocols::ANY) {
        Ok(_card) => {
            println!("successfully connected to {:?}", reader);
        }
        Err(Error::NoSmartcard) => {
            println!("SKIP: reader {:?} has no smart card inserted", reader);
        }
        Err(e) => {
            println!("SKIP: failed to connect to {:?}: {e}", reader);
        }
    }
}

#[test]
fn test_transmit_select_apdu() {
    let ctx = match Context::establish(Scope::User) {
        Ok(ctx) => ctx,
        Err(e) => {
            println!("SKIP: cannot establish PC/SC context: {e}");
            return;
        }
    };

    let readers = get_readers(&ctx);
    if readers.is_empty() {
        println!("SKIP: no PC/SC readers connected");
        return;
    }

    let reader = &readers[0];
    let card = match ctx.connect(reader, ShareMode::Shared, Protocols::ANY) {
        Ok(card) => card,
        Err(Error::NoSmartcard) => {
            println!("SKIP: reader {:?} has no smart card inserted", reader);
            return;
        }
        Err(e) => {
            println!("SKIP: failed to connect to {:?}: {e}", reader);
            return;
        }
    };

    let apdu: &[u8] = &[0x00, 0xA4, 0x04, 0x00, 0x00];
    let mut rapdu_buf = [0u8; MAX_BUFFER_SIZE];

    match card.transmit(apdu, &mut rapdu_buf) {
        Ok(rapdu) => {
            println!("RAPDU: {rapdu:02X?}");
            assert!(
                rapdu.len() >= 2,
                "APDU response too short: {} bytes (expected >= 2 for SW1/SW2)",
                rapdu.len()
            );
            let sw1 = rapdu[rapdu.len() - 2];
            let sw2 = rapdu[rapdu.len() - 1];
            println!("SW1=0x{sw1:02X} SW2=0x{sw2:02X}");
        }
        Err(e) => {
            println!("transmit error: {e} (acceptable for smoke test)");
        }
    }
}
