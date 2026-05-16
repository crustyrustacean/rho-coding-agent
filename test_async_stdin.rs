use tokio::io::{AsyncBufReadExt, BufReader};

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    
    println!("Enter something:");
    let line = reader.next_line().await.unwrap().unwrap();
    println!("Got: {}", line);
}
