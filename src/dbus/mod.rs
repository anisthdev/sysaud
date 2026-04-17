use zbus::Connection;

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
