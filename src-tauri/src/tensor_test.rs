#[cfg(test)]
mod tensor_tests {
    use super::*;
    use crate::models::schema::Signal;

    #[test]
    fn test_tensor_broadcasting_math() {
        // Signal A: Spatial (N=4, T=1, C=1) -> [0, 1, 2, 3]
        let sig_a = Signal {
            n: 4,
            t: 1,
            c: 1,
            data: vec![0.0, 1.0, 2.0, 3.0],
        };

        // Signal B: Temporal (N=1, T=2, C=1) -> [10, 20]
        let sig_b = Signal {
            n: 1,
            t: 2,
            c: 1,
            data: vec![10.0, 20.0],
        };

        // Expected Result: (N=4, T=2, C=1)
        // Row 0: [0+10, 0+20] = [10, 20]
        // Row 1: [1+10, 1+20] = [11, 21]
        // Row 2: [2+10, 2+20] = [12, 22]
        // Row 3: [3+10, 3+20] = [13, 23]
        
        let out_n = 4;
        let out_t = 2;
        let out_c = 1;
        let mut result_data = Vec::new();

        for i in 0..out_n {
            let idx_a_n = i % sig_a.n; // 0, 1, 2, 3
            let idx_b_n = 0;           // Always 0

            for j in 0..out_t {
                let idx_a_t = 0;       // Always 0
                let idx_b_t = j % sig_b.t; // 0, 1

                // Flat indices
                let flat_a = idx_a_n * (sig_a.t * sig_a.c) + idx_a_t * sig_a.c + 0;
                let flat_b = idx_b_n * (sig_b.t * sig_b.c) + idx_b_t * sig_b.c + 0;

                let val_a = sig_a.data[flat_a];
                let val_b = sig_b.data[flat_b];
                result_data.push(val_a + val_b);
            }
        }

        assert_eq!(result_data, vec![
            10.0, 20.0, 
            11.0, 21.0, 
            12.0, 22.0, 
            13.0, 23.0
        ]);
    }
}
