use tokio::io::{AsyncBufReadExt, BufReader};

#[tokio::main]
async fn main() {
    eprintln!("Test 1: Basic async stdin");
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    
    print!("Enter something: ");
    std::io::stdout().flush().unwrap();
    
    let line = reader.next_line().await.unwrap().unwrap();
    println!("Got: {}", line);
}
