# Init 0.4 design QA

final result: passed

The reviewed workspace, utilization and Settings flows have no remaining release-blocking design findings. This report covers the integrated Init interface, including the newer Machines, host monitor, GitHub deployment and service controls merged from main.

## Source and review boundary

The existing Init/Bit identity, charcoal surfaces, system typography, restrained green actions and blue memory plots remain the visual baseline. The workspace refinement uses the UI/UX Designer and Workbench skills: persistent object context, service worklists, a secondary execution/storage inspector, and separate workspace and controller navigation. This is an intentional functional extension, not a pixel copy of a reference.

Visual comparison used the earlier workspace interface at `work/design-monitor/final-workspaces-desktop.jpg` and the rendered integrated workspace and Settings captures under `work/workspace-v4/`. Source and implementation images were opened together. An early post-restart capture showed the loading state; it was replaced with a settled populated capture and is not used as evidence. The new layout replaces repetitive workspace cards with compact rows and a selected workspace view; its service list, group network and per-service storage context are real data.

Claude Sonnet 5.5 separately inspected the integrated Overview and Settings screenshots plus targeted frontend source through Cursor's read-only ask mode. Its verdict was **Ship**, with no P1 findings. The report and provenance are retained locally as `work/workspace-v4/claude-review.md` and `claude-review-provenance.json`. Its review did not exercise interactions, mobile layouts or a screen reader. The browser checks below were performed separately against the actual Rust server.

## Evidence

QA used the isolated controller at `http://127.0.0.1:18787`, its own configuration and history database, and actual read-only service observers. The production controller and its services were not changed by this UI QA. No disposable containers were created; the deployment wizard was validated through preview and cancelled before apply.

| Local evidence in `work/workspace-v4/` | Verified state |
| --- | --- |
| `final-workspace-desktop.jpg` | Populated Build tools workspace, service list, actual 24h history coverage, execution/network/storage context |
| `final-workspace-mobile.jpg` | Stacked workspace content and mobile navigation at 390 × 844 |
| `final-overview-desktop.jpg` | Preserved service status distribution/list with persistent utilization |
| `final-settings-desktop.jpg` | Controller setting, history readiness, local preferences and selected-controller paths |
| `final-settings-mobile.jpg` | Settings controls and long paths on mobile |
| `final-laptop-rail.jpg` | All desktop navigation and connection status visible at 1280 × 720 |
| `final-service-history.jpg` | Recorded service history and keyboard-selected older bucket |
| `final-host-monitor.jpg` | Preserved real host CPU, memory and network monitor |
| `final-deploy-review.jpg` | Real validated image, destination, limits and new private volume preview |

Desktop checks used 1440 × 1000, 1280 × 900 and 1280 × 720 CSS viewports. Mobile used 390 × 844; the vertical scrollbar reduces available content width to 375 px in long views. Long full-page captures place the fixed mobile navigation at the viewport's bottom position. Representative final workspace and Settings views are also copied into the task's `outputs/self-host-init-0.4/` folder.

## Findings fixed and rechecked

- **Wrong totals for aliases:** live workspace totals now deduplicate `telemetry_target`, select available CPU and memory readings independently, reject stale samples, and disclose partial target coverage. Missing metrics remain unknown. Pure helper checks cover aliases, missing values and actual zero readings.
- **Silent history collection errors:** an HTTP 200 history response with collection failure now shows the error alongside retained records. Fetch failures retain the previous graph with its loaded time and automatic retry status. A regression check exercises retained-history errors.
- **Wrong copied controller identity:** Settings commands include both shell-quoted workspace and configuration paths. Remote-controller help explicitly says to run those commands on that machine; they do not automatically run through SSH.
- **Navigation below laptop viewport:** the desktop rail uses a compact density rule at heights up to 800 px. At 1280 × 720 its client and scroll heights are both 720 px; Settings is at y545.5–579.5 and the footer at y620.5–708. No links are clipped. Activity and GitHub setup sit with workspace views; host monitor, Resources, Machines and Settings are controller-wide.
- **Job admission wording:** help now states that manual/deploy starts can be rejected when full, recurring jobs retry later, and running jobs continue.
- **Mixed chart statistical scopes:** window-wide observed average/min/max are labelled separately from individual bucket averages. Graph captions explicitly identify bucket averages; an observed spike can exceed the plotted average without implying a contradictory axis.
- **Clipped CLI commands:** the command block wraps complete workspace/configuration commands. Settings labels the VM/runtime row “Container engine location” to distinguish it from the controller computer.

## Interaction and data checks

- Workspace selection and return links retain Build tools through controller Settings and reload. The valid project ID `all` remains distinct from All workspaces. Its empty graphs show no recorded measurements, clear prior values, and do not draw zero traces.
- Settings saved the isolated controller's maximum parallel jobs, and the value survived reload. Light theme and a 30-second update interval also survived reload; charcoal and 5-second updates were restored afterward. Local preferences do not change the fixed 15-second history collection interval.
- The 24-hour window shows actual available records and missing earlier time. Captured records survived a controller restart. One-hour selection changed to one-minute buckets; weighted observed statistics, bounds, partial coverage and actual sample times were visible. No earlier measurements were backfilled.
- The service Usage tab loads its own persisted history. Home selected an older bucket; chart keyboard focus and the same selected timestamp survived real automatic state/history updates. Accessible sample tables remain inside fixed-size visually hidden wrappers, without extending the document.
- Host monitor remains independent of workspace filters and uses its own recent browser-session window. Its captions distinguish host percentages from service CPU percentages and Docker VM usage. Overview/workspace/service history is persisted by the selected daemon.
- GitHub setup and Machines routes, their existing forms, service status distribution, sorting and destination controls remain present. Frontend contract checks exercise generated workflow shell arguments, public GitHub deployment routing, machine proxy paths and pending-operation guards.
- The two-step deployment preview accepted a real container image in the selected named group, showed 0.25 CPU/256 MiB limits and a newly created private `/workspace` volume, and retained the non-root image permissions warning. Cancel returned to the underlying view without creating resources.
- Browser diagnostics were inspected for application errors. No application console errors were observed in the reviewed flows; unrelated installed-extension messages are outside this app's runtime.

## Visual and accessibility checks

- The selected workspace exposes services first, then recorded utilization, with execution, group networking, service storage and recent events in a secondary inspector. The inspector collapses below the main content before text becomes cramped.
- Mobile workspace and Settings document widths matched their available 375 px content area. Long service names and paths wrap; command text wraps locally. Forms, chart windows and navigation retain visible focus and usable targets. No page-level horizontal scrolling was observed in the reviewed views.
- Declared merged dark-theme contrast checks passed: text 12.43–15.17:1, secondary text 6.71–8.19:1, primary action 7.02:1, Settings input boundary 3.24:1, running status 6.13:1 and chart strokes 6.90–7.17:1. These are opaque token-pair calculations, not a complete rendered accessibility certification.
- Layout arithmetic checks passed at 1440, 1280, 1024, 768 and 390 px using the actual 232 px rail and declared content padding. Actual browser rendering was checked at the representative desktop/mobile sizes above; arithmetic alone does not certify font fitting.
- The existing frontend contract, host monitor, service ordering and GitHub workflow scripts pass. Added contract assertions cover history/settings proxy paths, deduplicated target totals, stale/missing/zero readings, weighted history bounds, history error status and workspace/global route separation.

## Limits

Real SSH connections were not created or tested during this pass. Machine switching uses a full navigation, so in-memory history/settings caches clear; each selected daemon owns its history database and settings. Proxy routing and revision handling are covered by contract/backend checks, rather than a claimed real remote-machine deployment.

This QA observed a growing history shorter than 24 hours and verified honest gaps; it did not wait a full day or simulate a day of measurements. Host-wide monitoring retains its documented browser-session history; persistent 24h graphs cover observed services. Full screen-reader and all-platform desktop certification were not performed. Resource enforcement remains available for managed Docker containers and opted-in Linux systemd units; existing Mac launchd services remain monitor-only.
