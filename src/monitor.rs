//! Controller-host telemetry, separate from Docker engine and service usage.
use anyhow::{Context, Result, ensure};
use serde::Serialize;
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Instant};

#[derive(Clone)]
struct CpuTicks {
    states: Vec<u64>,
    idle: u64,
}

fn cpu_percent(previous: &CpuTicks, current: &CpuTicks) -> Option<f64> {
    if previous.states.len() != current.states.len() {
        return None;
    }
    let total = current
        .states
        .iter()
        .zip(&previous.states)
        .try_fold(0_u64, |total, (current, previous)| {
            total.checked_add(current.checked_sub(*previous)?)
        })?;
    let idle = current.idle.checked_sub(previous.idle)?;
    (total > 0 && idle <= total).then(|| (total - idle) as f64 / total as f64 * 100.)
}

#[derive(Clone)]
struct NetworkCounters {
    rx: u64,
    tx: u64,
}

fn network_rates(
    previous: &NetworkCounters,
    current: &NetworkCounters,
    seconds: f64,
) -> Option<(f64, f64)> {
    if !seconds.is_finite() || seconds <= 0. {
        return None;
    }
    Some((
        current.rx.checked_sub(previous.rx)? as f64 / seconds,
        current.tx.checked_sub(previous.tx)? as f64 / seconds,
    ))
}

#[derive(Default, Serialize)]
struct Memory {
    total_bytes: Option<u64>,
    available_bytes: Option<u64>,
    used_bytes: Option<u64>,
    cached_bytes: Option<u64>,
    swap_total_bytes: Option<u64>,
    swap_used_bytes: Option<u64>,
    active_bytes: Option<u64>,
    inactive_bytes: Option<u64>,
    wired_bytes: Option<u64>,
    compressed_bytes: Option<u64>,
    available_basis: Option<&'static str>,
    cached_basis: Option<&'static str>,
}

struct Reading {
    captured_at: Instant,
    cpu: Option<BTreeMap<usize, CpuTicks>>,
    memory: Memory,
    network: Option<BTreeMap<String, NetworkCounters>>,
    errors: Vec<String>,
}

impl Default for Reading {
    fn default() -> Self {
        Self {
            captured_at: Instant::now(),
            cpu: None,
            memory: Memory::default(),
            network: None,
            errors: vec![],
        }
    }
}

#[derive(Default)]
pub struct HostMonitor {
    previous: Option<Reading>,
}

impl HostMonitor {
    pub async fn sample(&mut self) -> Value {
        self.render(read_host().await)
    }

    fn render(&mut self, mut reading: Reading) -> Value {
        let previous = self.previous.as_ref();
        let seconds = previous.map(|p| {
            reading
                .captured_at
                .duration_since(p.captured_at)
                .as_secs_f64()
        });
        let cores: Vec<_> = reading
            .cpu
            .iter()
            .flat_map(|cpu| cpu.iter())
            .map(|(id, current)| {
                let prior = previous.and_then(|p| p.cpu.as_ref()?.get(id));
                let percent = prior.and_then(|p| cpu_percent(p, current));
                if prior.is_some_and(|p| {
                    p.states
                        .iter()
                        .zip(&current.states)
                        .any(|(old, new)| new < old)
                }) {
                    reading.errors.push(format!(
                        "CPU {id} counters reset; waiting for a new interval"
                    ));
                }
                json!({"id":id,"percent":percent})
            })
            .collect();
        let percent = if cores.is_empty() || cores.iter().any(|core| core["percent"].is_null()) {
            None
        } else {
            Some(
                cores
                    .iter()
                    .filter_map(|core| core["percent"].as_f64())
                    .sum::<f64>()
                    / cores.len() as f64,
            )
        };
        let interfaces: Vec<_> = reading.network.iter().flat_map(|network| network.iter()).map(|(name,current)| {
            let prior = previous.and_then(|p| p.network.as_ref()?.get(name));
            let rates = prior.zip(seconds).and_then(|(p,seconds)| network_rates(p,current,seconds));
            if prior.is_some_and(|p| current.rx < p.rx || current.tx < p.tx) {
                reading.errors.push(format!("Network interface {name} counters reset; waiting for a new interval"));
            }
            json!({"name":name,"rx_bytes":current.rx,"tx_bytes":current.tx,"rx_bytes_per_second":rates.map(|r|r.0),"tx_bytes_per_second":rates.map(|r|r.1)})
        }).collect();
        let value = json!({
            "scope":"controller host",
            "label":if cfg!(target_os="macos") {"This Mac"} else {"This host"},
            "os":std::env::consts::OS,
            "sampled_at":crate::control::now(),
            "interval_seconds":seconds,
            "cpu":{"logical_cores":reading.cpu.as_ref().map(BTreeMap::len),"percent":percent,"cores":cores,"basis":"100% = all logical CPUs busy"},
            "memory":reading.memory,
            "network":{"interfaces":interfaces},
            "errors":reading.errors
        });
        // A failed collection clears that baseline so recovery never reports a stale rate.
        self.previous = Some(reading);
        value
    }
}

fn recorded<T>(result: Result<T>, label: &str, errors: &mut Vec<String>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            errors.push(format!("{label}: {error:#}"));
            None
        }
    }
}

#[cfg(target_os = "linux")]
async fn read_host() -> Reading {
    let mut reading = Reading::default();
    reading.cpu = recorded(
        std::fs::read_to_string("/proc/stat")
            .context("read /proc/stat")
            .and_then(|text| linux_cpu(&text)),
        "CPU",
        &mut reading.errors,
    );
    if let Some(memory) = recorded(
        std::fs::read_to_string("/proc/meminfo")
            .context("read /proc/meminfo")
            .and_then(|text| linux_memory(&text)),
        "Memory",
        &mut reading.errors,
    ) {
        reading.memory = memory;
    }
    reading.network = recorded(
        std::fs::read_to_string("/proc/net/dev")
            .context("read /proc/net/dev")
            .and_then(|text| linux_network(&text)),
        "Network",
        &mut reading.errors,
    );
    reading
}

#[cfg(any(target_os = "linux", test))]
fn linux_cpu(text: &str) -> Result<BTreeMap<usize, CpuTicks>> {
    let mut cores = BTreeMap::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let Some(id) = fields
            .next()
            .and_then(|name| name.strip_prefix("cpu"))
            .filter(|id| !id.is_empty())
        else {
            continue;
        };
        if !id.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let id = id.parse()?;
        // Linux includes guest/guest_nice in user/nice already; sum only the first eight states.
        let states: Vec<u64> = fields
            .take(8)
            .map(str::parse)
            .collect::<std::result::Result<_, _>>()?;
        ensure!(states.len() >= 4, "CPU {id} has incomplete counters");
        let idle = states[3]
            .checked_add(states.get(4).copied().unwrap_or(0))
            .context("idle counter overflow")?;
        ensure!(
            cores.insert(id, CpuTicks { states, idle }).is_none(),
            "duplicate CPU {id}"
        );
    }
    ensure!(!cores.is_empty(), "no logical CPU counters found");
    Ok(cores)
}

#[cfg(any(target_os = "linux", test))]
fn linux_memory(text: &str) -> Result<Memory> {
    let mut fields = BTreeMap::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let mut parts = value.split_whitespace();
        if let (Some(value), Some("kB")) = (parts.next(), parts.next()) {
            fields.insert(
                key,
                value
                    .parse::<u64>()?
                    .checked_mul(1024)
                    .context("memory counter overflow")?,
            );
        }
    }
    let total = fields
        .get("MemTotal")
        .copied()
        .context("MemTotal missing")?;
    let available = fields.get("MemAvailable").copied();
    ensure!(
        available.is_none_or(|value| value <= total),
        "available memory exceeds total"
    );
    let swap_total = fields.get("SwapTotal").copied();
    let swap_used = swap_total
        .zip(fields.get("SwapFree"))
        .map(|(total, free)| total.checked_sub(*free).context("swap free exceeds total"))
        .transpose()?;
    Ok(Memory {
        total_bytes: Some(total),
        available_bytes: available,
        used_bytes: available.map(|available| total - available),
        cached_bytes: fields.get("Cached").copied(),
        swap_total_bytes: swap_total,
        swap_used_bytes: swap_used,
        active_bytes: fields.get("Active").copied(),
        inactive_bytes: fields.get("Inactive").copied(),
        available_basis: Some("Linux MemAvailable estimate"),
        cached_basis: Some("Linux Cached page cache, including tmpfs/shared memory"),
        ..Memory::default()
    })
}

#[cfg(any(target_os = "linux", test))]
fn linux_network(text: &str) -> Result<BTreeMap<String, NetworkCounters>> {
    let mut interfaces = BTreeMap::new();
    for line in text.lines().skip(2) {
        let Some((name, counters)) = line.split_once(':') else {
            continue;
        };
        let fields: Vec<_> = counters.split_whitespace().collect();
        ensure!(
            fields.len() >= 16,
            "incomplete network counters for {}",
            name.trim()
        );
        interfaces.insert(
            name.trim().to_owned(),
            NetworkCounters {
                rx: fields[0].parse()?,
                tx: fields[8].parse()?,
            },
        );
    }
    ensure!(!interfaces.is_empty(), "no interface counters found");
    Ok(interfaces)
}

#[cfg(target_os = "macos")]
async fn read_host() -> Reading {
    let mut reading = Reading::default();
    reading.cpu = recorded(macos_cpu(), "CPU", &mut reading.errors);
    let (total, vm, swap, network) = tokio::join!(
        async {
            crate::runtime::run(&["/usr/sbin/sysctl".into(), "-n".into(), "hw.memsize".into()])
                .await
        },
        async { crate::runtime::run(&["/usr/bin/vm_stat".into()]).await },
        async { crate::runtime::run(&["/usr/sbin/sysctl".into(), "vm.swapusage".into()]).await },
        async { crate::runtime::run(&["/usr/sbin/netstat".into(), "-ibn".into()]).await }
    );
    reading.memory.total_bytes = recorded(
        total.and_then(|text| text.trim().parse().context("invalid hw.memsize")),
        "Memory total",
        &mut reading.errors,
    );
    if let Some(memory) = recorded(
        vm.and_then(|text| macos_memory(&text, reading.memory.total_bytes)),
        "Memory pages",
        &mut reading.errors,
    ) {
        reading.memory = memory;
    }
    if let Some((total, used)) = recorded(
        swap.and_then(|text| macos_swap(&text)),
        "Swap",
        &mut reading.errors,
    ) {
        reading.memory.swap_total_bytes = Some(total);
        reading.memory.swap_used_bytes = Some(used);
    }
    reading.network = recorded(
        network.and_then(|text| macos_network(&text)),
        "Network",
        &mut reading.errors,
    );
    reading
}

#[cfg(target_os = "macos")]
#[allow(deprecated)] // libc's Mach bindings use the stable OS ABI without another bindings crate.
fn macos_cpu() -> Result<BTreeMap<usize, CpuTicks>> {
    // Retain one host send right for the process lifetime instead of leaking one per sample.
    static HOST: std::sync::OnceLock<libc::mach_port_t> = std::sync::OnceLock::new();
    let host = *HOST.get_or_init(|| unsafe { libc::mach_host_self() });
    let mut cpu_count = 0;
    let mut count = 0;
    let mut info = std::ptr::null_mut();
    // The kernel allocates `count` integer_t elements; copy before releasing its VM allocation.
    let status = unsafe {
        libc::host_processor_info(
            host,
            libc::PROCESSOR_CPU_LOAD_INFO,
            &mut cpu_count,
            &mut info,
            &mut count,
        )
    };
    ensure!(
        status == libc::KERN_SUCCESS,
        "host_processor_info failed: {status}"
    );
    let valid = !info.is_null()
        && cpu_count > 0
        && count as usize == cpu_count as usize * libc::CPU_STATE_MAX as usize;
    let mut cores = BTreeMap::new();
    if valid {
        let ticks = unsafe { std::slice::from_raw_parts(info, count as usize) };
        for (id, states) in ticks
            .as_chunks::<{ libc::CPU_STATE_MAX as usize }>()
            .0
            .iter()
            .enumerate()
        {
            let states: Vec<_> = states.iter().map(|value| *value as u32 as u64).collect();
            let idle = states[libc::CPU_STATE_IDLE as usize];
            cores.insert(id, CpuTicks { states, idle });
        }
    }
    let released = if info.is_null() {
        libc::KERN_SUCCESS
    } else {
        unsafe {
            libc::vm_deallocate(
                libc::mach_task_self(),
                info as libc::vm_address_t,
                count as libc::vm_size_t
                    * std::mem::size_of::<libc::integer_t>() as libc::vm_size_t,
            )
        }
    };
    ensure!(
        released == libc::KERN_SUCCESS,
        "CPU sample VM deallocation failed: {released}"
    );
    ensure!(valid, "host_processor_info returned incomplete counters");
    Ok(cores)
}

#[cfg(any(target_os = "macos", test))]
fn macos_memory(text: &str, total: Option<u64>) -> Result<Memory> {
    let page_size = text
        .lines()
        .next()
        .and_then(|line| {
            line.split_once("page size of ")?
                .1
                .split_whitespace()
                .next()
        })
        .context("vm_stat page size missing")?
        .parse::<u64>()?;
    ensure!(page_size > 0, "invalid vm_stat page size");
    let mut pages = BTreeMap::new();
    for line in text.lines().skip(1) {
        if let Some((name, value)) = line.split_once(':') {
            pages.insert(name, value.trim().trim_end_matches('.').parse::<u64>()?);
        }
    }
    let bytes = |name| -> Result<Option<u64>> {
        pages
            .get(name)
            .map(|value| {
                value
                    .checked_mul(page_size)
                    .context("memory counter overflow")
            })
            .transpose()
    };
    let available = bytes("Pages free")?
        .zip(bytes("Pages inactive")?)
        .zip(bytes("Pages speculative")?)
        .and_then(|((free, inactive), speculative)| {
            free.checked_add(inactive)?.checked_add(speculative)
        });
    ensure!(
        available
            .zip(total)
            .is_none_or(|(available, total)| available <= total),
        "available memory exceeds total"
    );
    Ok(Memory {
        total_bytes: total,
        available_bytes: available,
        used_bytes: total
            .zip(available)
            .map(|(total, available)| total - available),
        cached_bytes: bytes("File-backed pages")?,
        active_bytes: bytes("Pages active")?,
        inactive_bytes: bytes("Pages inactive")?,
        wired_bytes: bytes("Pages wired down")?,
        compressed_bytes: bytes("Pages occupied by compressor")?,
        available_basis: Some("macOS estimate: free + inactive + speculative pages"),
        cached_basis: Some("macOS file-backed pages; may overlap other categories"),
        ..Memory::default()
    })
}

#[cfg(any(target_os = "macos", test))]
fn macos_swap(text: &str) -> Result<(u64, u64)> {
    let bytes = |key| -> Result<u64> {
        let number = text
            .split_once(key)
            .and_then(|(_, tail)| tail.split_whitespace().next())
            .context("swap counter missing")?;
        let (value, unit) =
            number.split_at(number.len().checked_sub(1).context("empty swap counter")?);
        let multiplier = match unit {
            "K" => 1024.,
            "M" => 1048576.,
            "G" => 1073741824.,
            _ => anyhow::bail!("unknown swap unit"),
        };
        let value = value.parse::<f64>()? * multiplier;
        ensure!(
            value.is_finite() && value >= 0. && value < u64::MAX as f64,
            "invalid swap counter"
        );
        Ok(value as u64)
    };
    let (total, used) = (bytes("total = ")?, bytes("used = ")?);
    ensure!(used <= total, "swap used exceeds total");
    Ok((total, used))
}

#[cfg(any(target_os = "macos", test))]
fn macos_network(text: &str) -> Result<BTreeMap<String, NetworkCounters>> {
    let mut interfaces = BTreeMap::new();
    for line in text.lines().skip(1) {
        let fields: Vec<_> = line.split_whitespace().collect();
        if !fields
            .get(2)
            .is_some_and(|network| network.starts_with("<Link#"))
        {
            continue;
        }
        // Link rows have no Address cell on loopback and tunnel interfaces. Count back from Coll.
        ensure!(fields.len() >= 10, "incomplete link counters");
        let last = fields.len();
        interfaces.insert(
            fields[0].trim_end_matches('*').to_owned(),
            NetworkCounters {
                rx: fields[last - 5].parse()?,
                tx: fields[last - 2].parse()?,
            },
        );
    }
    ensure!(!interfaces.is_empty(), "no link interface counters found");
    Ok(interfaces)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
async fn read_host() -> Reading {
    Reading {
        errors: vec!["Host monitoring is supported on Linux and macOS".into()],
        ..Reading::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn linux_cpu_ignores_guest_and_requires_fresh_monotonic_counters() {
        let before =
            linux_cpu("cpu 0 0 0 0\ncpu0 100 0 10 200 0 0 0 0 75 0\ncpu1 10 0 10 80\n").unwrap();
        let after = linux_cpu("cpu0 125 0 35 250 0 0 0 0 100 0\ncpu1 20 0 20 160\n").unwrap();
        assert_eq!(cpu_percent(&before[&0], &after[&0]), Some(50.));
        assert_eq!(cpu_percent(&before[&1], &after[&1]), Some(20.));
        assert_eq!(cpu_percent(&after[&0], &before[&0]), None);
        assert_eq!(cpu_percent(&before[&0], &before[&0]), None);
        assert!(linux_cpu("cpu0 1 2\n").is_err());
    }

    #[test]
    fn memory_parsers_respect_units_and_keep_unknown_values_null() {
        let memory=linux_memory("MemTotal: 1000 kB\nMemAvailable: 250 kB\nCached: 100 kB\nSwapTotal: 200 kB\nSwapFree: 50 kB\n").unwrap();
        assert_eq!(memory.total_bytes, Some(1024000));
        assert_eq!(memory.used_bytes, Some(768000));
        assert_eq!(memory.swap_used_bytes, Some(153600));
        assert!(linux_memory("MemTotal: 1 kB\nMemAvailable: 2 kB\n").is_err());
        assert!(
            linux_memory("MemTotal: 1000 kB")
                .unwrap()
                .used_bytes
                .is_none()
        );
        let memory=macos_memory("Mach Virtual Memory Statistics: (page size of 16384 bytes)\nPages free: 2.\nPages inactive: 3.\nPages speculative: 1.\nPages occupied by compressor: 4.\nFile-backed pages: 5.\n",Some(163840)).unwrap();
        assert_eq!(memory.available_bytes, Some(98304));
        assert_eq!(memory.used_bytes, Some(65536));
        assert_eq!(memory.compressed_bytes, Some(65536));
        assert_eq!(
            macos_swap("vm.swapusage: total = 2.00M  used = 1.50M  free = 0.50M").unwrap(),
            (2097152, 1572864)
        );
        assert!(macos_swap("total = NaNM used = 1M").is_err());
    }

    #[test]
    fn network_parsers_select_link_rows_once_and_preserve_64_bit_counters() {
        let mac=macos_network("Name Mtu Network Address Ipkts Ierrs Ibytes Opkts Oerrs Obytes Coll\nlo0 16384 <Link#1> 2 0 100 2 0 200 0\nen0 1500 <Link#2> aa:bb:cc 3 0 5000000000 4 0 6000000000 0\nen0 1500 192.168.1 192.168.1.2 3 - 5000000000 4 - 6000000000 -\nutun0* 1380 <Link#3> 1 0 50 1 0 60 0\n").unwrap();
        assert_eq!(mac.len(), 3);
        assert_eq!(mac["en0"].rx, 5000000000);
        assert_eq!(mac["lo0"].tx, 200);
        assert_eq!(mac["utun0"].rx, 50);
        let linux=linux_network("Inter-| Receive | Transmit\n face |bytes packets errs drop fifo frame compressed multicast|bytes packets errs drop fifo colls carrier compressed\n eth0: 5000000000 1 0 0 0 0 0 0 6000000000 2 0 0 0 0 0 0\n").unwrap();
        assert_eq!(linux["eth0"].tx, 6000000000);
        assert!(linux_network("header\nheader\n eth0: 1 2\n").is_err());
    }

    fn reading(at: Instant, busy: u64, idle: u64, rx: u64, tx: u64) -> Reading {
        Reading {
            captured_at: at,
            cpu: Some(BTreeMap::from([(
                0,
                CpuTicks {
                    states: vec![busy, idle],
                    idle,
                },
            )])),
            network: Some(BTreeMap::from([(
                "eth0".into(),
                NetworkCounters { rx, tx },
            )])),
            ..Reading::default()
        }
    }

    #[test]
    fn snapshots_need_baseline_and_recover_after_resets_or_collection_failures() {
        let mut monitor = HostMonitor::default();
        let start = Instant::now();
        let first = monitor.render(reading(start, 10, 10, 100, 200));
        assert!(first["cpu"]["percent"].is_null());
        assert!(first["network"]["interfaces"][0]["rx_bytes_per_second"].is_null());
        let next = monitor.render(reading(start + Duration::from_secs(2), 30, 70, 140, 260));
        assert_eq!(next["cpu"]["percent"], 25.);
        assert_eq!(next["network"]["interfaces"][0]["rx_bytes_per_second"], 20.);
        assert_eq!(next["network"]["interfaces"][0]["tx_bytes_per_second"], 30.);
        let reset = monitor.render(reading(start + Duration::from_secs(4), 1, 1, 1, 1));
        assert!(reset["cpu"]["percent"].is_null());
        assert!(reset["network"]["interfaces"][0]["rx_bytes_per_second"].is_null());
        assert_eq!(reset["errors"].as_array().unwrap().len(), 2);
        let recovered = monitor.render(reading(start + Duration::from_secs(6), 11, 11, 21, 41));
        assert_eq!(recovered["cpu"]["percent"], 50.);
        monitor.render(Reading {
            captured_at: start + Duration::from_secs(8),
            errors: vec!["collection failed".into()],
            ..Reading::default()
        });
        let after_failure =
            monitor.render(reading(start + Duration::from_secs(10), 21, 21, 41, 81));
        assert!(after_failure["cpu"]["percent"].is_null());
        assert!(after_failure["network"]["interfaces"][0]["rx_bytes_per_second"].is_null());
        assert!(
            network_rates(
                &NetworkCounters { rx: 1, tx: 1 },
                &NetworkCounters { rx: 2, tx: 2 },
                0.
            )
            .is_none()
        );
    }
}
