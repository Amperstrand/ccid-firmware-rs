use pcsc::{Context, Error, Scope};

pub fn list_readers() -> Result<Vec<String>, Error> {
    let ctx = Context::establish(Scope::User)?;
    let cstrings = ctx.list_readers_owned()?;
    let readers: Vec<String> = cstrings
        .into_iter()
        .filter_map(|cs| cs.into_string().ok())
        .collect();
    Ok(readers)
}
