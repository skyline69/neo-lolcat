use std::io::Write;
use std::process::{Command, Stdio};

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_lolcat")
}

#[test]
fn random_binary_input_does_not_crash() {
    const ITER: usize = 3;
    const CHUNK: usize = 64 * 1024;

    for seed in 0..ITER {
        let mut child = Command::new(binary())
            .args(["-f", "--spread", "5.0", "--freq", "0.15"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn neo-lolcat");

        {
            let mut stdin = child.stdin.take().expect("missing stdin");
            let mut rng = Lcg::new(seed as u64 + 1);
            let mut buffer = vec![0u8; CHUNK];
            for _ in 0..8 {
                rng.fill_bytes(&mut buffer);
                stdin.write_all(&buffer).expect("write failed");
            }
        }

        let status = child.wait().expect("failed to wait on child");
        assert!(status.success(), "neo-lolcat exited with {status:?}");
    }
}

struct Lcg {
    state: u64,
}

impl Lcg {
    fn new(seed: u64) -> Self {
        Self { state: seed.max(1) }
    }

    fn next_u32(&mut self) -> u32 {
        self.state = self.state.wrapping_mul(6364136223846793005).wrapping_add(1);
        (self.state >> 32) as u32
    }

    fn fill_bytes(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(4) {
            let rnd = self.next_u32().to_le_bytes();
            let len = chunk.len();
            chunk.copy_from_slice(&rnd[..len]);
        }
    }
}
