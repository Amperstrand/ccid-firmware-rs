use pcsc::{Context, Scope};

fn main() {
    let ctx = match Context::establish(Scope::User) {
        Ok(ctx) => ctx,
        Err(e) => {
            eprintln!("failed to establish PC/SC context: {e}");
            std::process::exit(1);
        }
    };

    let readers = match ctx.list_readers_owned() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("failed to list readers: {e}");
            std::process::exit(1);
        }
    };

    if readers.is_empty() {
        println!("no readers connected");
        return;
    }

    for reader in &readers {
        println!("{:?}", reader);
    }
}
