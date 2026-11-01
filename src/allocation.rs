//! Bounded weighted sharing. Floors are reserved first; spare capacity is shared
//! by weight and redistributed when a service reaches its declared ceiling.

use anyhow::{Result, ensure};
use serde::Serialize;

pub struct AllocationInput {
    pub id: String,
    pub weight: f64,
    pub min_cpus: f64,
    pub min_memory_mb: u64,
    pub max_cpus: f64,
    pub max_memory_mb: u64,
    pub memory_usage_mb: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Allocation {
    pub id: String,
    pub cpus: f64,
    pub memory_mb: u64,
    pub weight: f64,
    pub min_cpus: f64,
    pub min_memory_mb: u64,
}

// Integer units avoid granting more than the pool due to floating-point rounding.
fn share(pool: u64, minimum: &[u64], maximum: &[u64], weights: &[f64]) -> Result<Vec<u64>> {
    let reserved: u128 = minimum.iter().map(|n| *n as u128).sum();
    ensure!(
        reserved <= pool as u128,
        "Allocation minimums need {reserved} units but only {pool} are available; raise the budget or lower service minimums"
    );
    let mut result = minimum.to_vec();
    let mut remaining = pool - reserved as u64;
    while remaining > 0 {
        let active: Vec<_> = (0..result.len())
            .filter(|i| result[*i] < maximum[*i])
            .collect();
        let total_weight: f64 = active.iter().map(|i| weights[*i]).sum();
        if active.is_empty() {
            break;
        }
        let mut granted = 0;
        for i in &active {
            let amount = ((remaining as f64 * weights[*i] / total_weight).floor() as u64)
                .min(maximum[*i] - result[*i])
                .min(remaining - granted);
            result[*i] += amount;
            granted += amount;
        }
        if granted == 0 {
            break;
        }
        remaining -= granted;
    }
    Ok(result)
}

pub fn allocate(inputs: &[AllocationInput], cpus: f64, memory_mb: u64) -> Result<Vec<Allocation>> {
    ensure!(
        cpus.is_finite() && cpus >= 0.,
        "Invalid CPU allocation pool"
    );
    let mut cpu_min = vec![];
    let mut cpu_max = vec![];
    let mut memory_min = vec![];
    let mut memory_max = vec![];
    let mut weights = vec![];
    for input in inputs {
        ensure!(
            input.weight.is_finite() && input.weight > 0. && input.weight <= 1_000_000.,
            "{}: invalid allocation weight",
            input.id
        );
        ensure!(
            input.min_cpus.is_finite()
                && input.max_cpus.is_finite()
                && input.min_cpus >= 0.01
                && input.max_cpus <= 65536.
                && input.max_cpus >= input.min_cpus,
            "{}: invalid CPU allocation range",
            input.id
        );
        let floor = if let Some(usage) = input.memory_usage_mb {
            ensure!(
                usage.is_finite() && usage >= 0.,
                "{}: invalid memory telemetry",
                input.id
            );
            input.min_memory_mb.max((usage * 1.25).ceil() as u64)
        } else {
            input.min_memory_mb
        };
        ensure!(
            input.min_memory_mb >= 6 && floor <= input.max_memory_mb,
            "{}: memory usage plus 25% headroom needs {floor} MiB, above its {} MiB ceiling; raise the ceiling or reduce the workload",
            input.id,
            input.max_memory_mb
        );
        let minimum = (input.min_cpus * 1000.).ceil() as u64;
        let maximum = (input.max_cpus * 1000.).floor() as u64;
        ensure!(
            minimum <= maximum,
            "{}: CPU floor cannot fit at millicore precision",
            input.id
        );
        cpu_min.push(minimum);
        cpu_max.push(maximum);
        memory_min.push(floor);
        memory_max.push(input.max_memory_mb);
        weights.push(input.weight);
    }
    let cpu = share((cpus * 1000.).floor() as u64, &cpu_min, &cpu_max, &weights)?;
    let memory = share(memory_mb, &memory_min, &memory_max, &weights)?;
    Ok(inputs
        .iter()
        .enumerate()
        .map(|(i, input)| Allocation {
            id: input.id.clone(),
            cpus: cpu[i] as f64 / 1000.,
            memory_mb: memory[i],
            weight: input.weight,
            min_cpus: input.min_cpus,
            min_memory_mb: memory_min[i],
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn item(id: &str, weight: f64) -> AllocationInput {
        AllocationInput {
            id: id.into(),
            weight,
            min_cpus: 0.1,
            min_memory_mb: 64,
            max_cpus: 4.,
            max_memory_mb: 4096,
            memory_usage_mb: None,
        }
    }
    #[test]
    fn weighted_share_and_ceiling_redistribution() {
        let mut inputs = vec![item("a", 1.), item("b", 3.)];
        let result = allocate(&inputs, 2.2, 1024).unwrap();
        assert_eq!((result[0].cpus, result[1].cpus), (0.6, 1.6));
        inputs[1].max_cpus = 0.4;
        let result = allocate(&inputs, 2., 1024).unwrap();
        assert_eq!((result[0].cpus, result[1].cpus), (1.6, 0.4));
    }
    #[test]
    fn protects_live_memory_and_rejects_impossible_floors() {
        let mut inputs = vec![item("a", 1.), item("b", 1.)];
        inputs[0].memory_usage_mb = Some(400.);
        let result = allocate(&inputs, 1., 1024).unwrap();
        assert!(result[0].memory_mb >= 500);
        assert!(allocate(&inputs, 1., 512).is_err());
        inputs[0].max_memory_mb = 450;
        assert!(
            allocate(&inputs, 1., 1024)
                .unwrap_err()
                .to_string()
                .contains("headroom")
        );
        assert!(allocate(&inputs, 0.1, 1024).is_err());
    }
    #[test]
    fn rounding_stays_inside_pool_and_ceilings() {
        let inputs: Vec<_> = (0..37)
            .map(|i| item(&i.to_string(), (i + 1) as f64))
            .collect();
        let result = allocate(&inputs, 3.7777, 4567).unwrap();
        assert!(result.iter().map(|r| r.cpus).sum::<f64>() <= 3.7777);
        assert!(result.iter().map(|r| r.memory_mb).sum::<u64>() <= 4567);
        for row in result {
            assert!(row.cpus >= 0.1 && row.cpus <= 4. && row.memory_mb >= 64);
        }
    }
    #[test]
    fn invalid_telemetry_and_weights_never_reach_the_allocator() {
        let mut input = item("a", f64::NAN);
        assert!(allocate(&[input], 1., 1024).is_err());
        input = item("a", 1.);
        input.memory_usage_mb = Some(f64::NAN);
        assert!(allocate(&[input], 1., 1024).is_err());
        assert!(allocate(&[], 1., 1024).unwrap().is_empty());
    }
}
