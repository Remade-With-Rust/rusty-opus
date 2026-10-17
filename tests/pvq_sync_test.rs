use rusty_opus::pvq::{cwrsi, icwrs};

#[test]
fn test_pvq_sync() {
    let n = 20u32;
    let k = 10u32;
    let mut y = vec![0i32; n as usize];
    y[0] = 5;
    y[5] = -3;
    y[19] = 2;

    let index = icwrs(n, k, &y);
    println!("Vector: {y:?}");
    println!("N={n}, K={k}, Index={index}");

    let mut y2 = vec![0i32; n as usize];
    cwrsi(n, k, index, &mut y2);
    println!("Decoded: {y2:?}");

    assert_eq!(y, y2, "PVQ Sync Failure!");
    println!("PVQ Sync Success for N=20, K=10");

    let k = 10u32;
    let n = 10u32;
    let mut y = vec![0i32; n as usize];
    y[0] = 5;
    y[5] = -3;
    y[9] = 2;
    let index = icwrs(n, k, &y);
    println!("N={n}, K={k}, Index (big K)={index}");
    let mut y2 = vec![0i32; n as usize];
    cwrsi(n, k, index, &mut y2);
    assert_eq!(y, y2, "PVQ Sync Failure (big K)!");
    println!("PVQ Sync Success for N=10, K=10");
}

fn main() {}
