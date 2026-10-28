//! Matrix parameters for system channel conversion, in canonical WAVE order.
//! Rows are input channels; columns are output channels (CoreAudio convention).
use anyhow::{Result, ensure};
pub fn matrix(input: u16, output: u16) -> Result<Vec<f32>> {
    ensure!(
        (1..=6).contains(&input) && (1..=2).contains(&output),
        "unsupported speaker matrix"
    );
    let mut left = vec![0.; input as usize];
    let mut right = left.clone();
    if input == 1 {
        left[0] = 1.;
        right[0] = 1.;
    } else {
        left[0] = 1.;
        right[1] = 1.;
        let q = std::f32::consts::FRAC_1_SQRT_2;
        if input == 3 || input == 5 || input == 6 {
            left[2] = q;
            right[2] = q;
        }
        if input == 6 {
            left[3] = 0.5;
            right[3] = 0.5;
            left[4] = q;
            right[5] = q;
        }
        if input == 5 {
            left[3] = q;
            right[4] = q;
        }
        if input == 4 {
            left[2] = q;
            right[3] = q;
        }
        let l = left.iter().sum::<f32>();
        let r = right.iter().sum::<f32>();
        for value in &mut left {
            *value /= l;
        }
        for value in &mut right {
            *value /= r;
        }
    }
    Ok((0..input as usize)
        .flat_map(|i| {
            if output == 1 {
                vec![(left[i] + right[i]) * 0.5]
            } else {
                vec![left[i], right[i]]
            }
        })
        .collect())
}
