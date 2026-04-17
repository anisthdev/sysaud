use zbus::{Connection, MatchRule, MessageStream};

pub async fn session_bus() -> anyhow::Result<Connection> {
    let conn = Connection::session().await?;
    tracing::debug!("Session D-Bus connected");
    Ok(conn)
}

pub async fn system_bus() -> anyhow::Result<Connection> {
    let conn = Connection::system().await?;
    tracing::debug!("System D-Bus connected");
    Ok(conn)
}

pub async fn matched_stream<'a>(
    conn: &'a Connection,
    rules: impl IntoIterator<Item = MatchRule<'a>>,
) -> anyhow::Result<MessageStream> {
    register_match_rules(conn, rules).await?;
    Ok(MessageStream::from(conn))
}

pub async fn register_match_rules<'a>(
    conn: &'a Connection,
    rules: impl IntoIterator<Item = MatchRule<'a>>,
) -> anyhow::Result<()> {
    let proxy = zbus::fdo::DBusProxy::new(conn).await?;
    for rule in rules {
        proxy.add_match_rule(rule).await?;
    }
    Ok(())
}
