# Host monitoring

Open **Host monitor** in the web or desktop console for a btop-style view of the selected controller's machine: total and per-core CPU, physical memory, swap, and network interfaces. Select another tested machine through **Machines** to inspect that controller's host instead. No `btop` installation is required.

## Read CPU and memory

Each CPU bar represents a **logical core** reported by the operating system. A core's 100% means that logical CPU was fully busy during the sampling interval. Total host CPU is the average across logical cores, from 0–100%. For example, one fully busy logical CPU out of eight is roughly 12.5% host CPU. These bars do not assign services to dedicated cores.

Service CPU uses a different scale: 100% is one core's worth of CPU time, so a service using several cores can exceed 100%. Service reservations and configured ceilings appear under **Resources**; they are distinct from measured host usage.

Memory uses binary units such as MiB and GiB. Used memory is total minus available memory; cache, swap, and the operating system's other categories provide additional context:

- **Linux:** available memory uses the kernel's `MemAvailable` estimate. Cache uses `Cached`, including page cache and tmpfs/shared-memory pages.
- **macOS:** available memory is an estimate from free, inactive, and speculative pages. Cache uses file-backed pages. Active, inactive, wired, and compressed categories can overlap; do not add them together as separate portions of total RAM.
- **Swap:** total and used come from the operating system. Swap use is separate from physical RAM use.

Linux CPU sampling uses `/proc/stat`; idle includes I/O wait, and guest CPU is not counted twice. Other host statistics come from Linux procfs or the macOS host APIs and system tools. Collection failures are shown explicitly instead of appearing as zero usage.

## Read network traffic

Each interface shows receive/transmit byte totals and rates in bytes per second. Rates are differences between successive samples; totals are the operating system's cumulative counters. Interfaces include physical adapters, loopback, bridges, and tunnels such as Tailscale. Traffic may pass through several interfaces, so adding their totals can count the same traffic more than once. These measurements cover host interfaces; they are not per-service or per-process traffic attribution.

The first sample needs a previous reading before CPU/rates are available. A new interface, counter reset, or failed collection can also leave a rate temporarily pending. The next valid pair of samples restores it. Pending and unavailable values are not a measured zero.

## Sampling and machine boundaries

The console polls every five seconds by default; Settings can change this to 10 or 30 seconds. **Whole-host** chart history stays in the browser for up to one hour and resets on a reload or machine switch. Service charts in Overview, Workspaces, and service details use a separate daemon-recorded 24-hour history, including while the window is closed. See [service utilization history](configuration.md#controller-settings-and-utilization-history).

Host monitor always measures the OS running that controller. On macOS with Docker Desktop or Lima, it measures the **Mac**, while container statistics and resource limits refer to the **Docker VM**. A controller running inside a Linux VM measures that guest's visible CPUs, memory, and interfaces. Host usage includes workloads outside Init's manifest and budget.

If a panel is unavailable, inspect the displayed collection error and confirm the controller is running on Linux or macOS with its normal OS tools and readable host statistics. Unsupported platforms return unavailable fields and an explanation. Use **Services** and **Logs** to connect a host spike with workload activity; host metrics alone do not identify the process responsible.
