mod test_dir;

use std::io;

use ployzd::corrosion::AdminClient;
use serde_json::json;
use test_dir::TestDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixListener,
};

#[tokio::test]
async fn admin_client_uses_big_endian_length_framed_json() {
    let root = TestDir::new("corrosion-admin");
    std::fs::create_dir_all(&root.0).unwrap();
    let path = root.0.join("admin.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let command = read_frame(&mut stream).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&command).unwrap(),
            json!({"Cluster": "MembershipStates"})
        );
        write_frame(&mut stream, br#"{"Json":{"state":"Alive"}}"#)
            .await
            .unwrap();
        write_frame(&mut stream, br#""Success""#).await.unwrap();
    });

    let response = AdminClient::new(path)
        .command(&json!({"Cluster": "MembershipStates"}))
        .await
        .unwrap();
    assert_eq!(response, vec![json!({"state": "Alive"})]);
    server.await.unwrap();
}

async fn read_frame(stream: &mut tokio::net::UnixStream) -> io::Result<Vec<u8>> {
    let length = stream.read_u32().await?;
    let mut data = vec![0; length as usize];
    stream.read_exact(&mut data).await?;
    Ok(data)
}

async fn write_frame(stream: &mut tokio::net::UnixStream, data: &[u8]) -> io::Result<()> {
    stream.write_u32(data.len() as u32).await?;
    stream.write_all(data).await
}
