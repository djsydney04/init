# Dashboard design QA

final result: passed

No actionable P0/P1/P2 findings remain in the reviewed interface. Final polling retained chart keyboard focus and the selected actual sample. The narrow document remained 375 × 1349 CSS px at both 2 and 29 actual samples; accessible table wrappers stayed 1 px high.

## Source and implementation

The source visual is `work/design-monitor/before.jpg` (1326 × 1155 px). Claude reviewed this interface and the actual HTML/CSS/JavaScript, producing `work/claude-redesign/claude-opus-design.md`, `work/claude-redesign/claude-design.md`, and the workspace scope addendum `work/claude-redesign/claude-workspaces.md`. The user requested a broader redesign, rather than pixel fidelity to the previous screen.

The before capture, the revised Services screen, the rendered Activity Monitor, and focused service charts were opened together in one image comparison input. This establishes the intended charcoal palette, native typography, restrained panels, and navigation rhythm while explicitly allowing new views, execution locations, workspace grouping, and actual usage graphs. The previous screenshot shows nine declarations; the new Services capture includes the real disposable worker. The monitor and service detail captures are additional states. No pixel-perfect or equal-density comparison is claimed across these different states. Focused deployment and service detail captures were also inspected at their original dimensions. The final full-view comparison was repeated after the wrapping, scale, heading, focus, and screen-reader table fixes; the clean revised captures showed no remaining actionable P0/P1/P2 difference.

Implementation: `http://127.0.0.1:18787/`, isolated disposable manifest. Existing services were observed read-only. Browser testing created a disposable named workspace, then deployed one real node worker with its own volume and network. No demonstration metrics were injected.

| Evidence | Dimensions | State |
| --- | --- | --- |
| `work/design-monitor/final-services-desktop.jpg` | 1265 × 1599 px | All services and actual execution locations |
| `work/design-monitor/final-monitor-desktop.jpg` | 1280 × 900 px | Named workspace, actual measured CPU/memory |
| `work/design-monitor/final-service-usage-desktop.jpg` | 1280 × 900 px | Service history and runtime limit |
| `work/design-monitor/final-logs-desktop.jpg` | 1280 × 900 px | Automatic real heartbeat logs |
| `work/design-monitor/deploy-review-desktop.jpg` | 1280 × 900 px | Validated real deployment preview |
| `work/design-monitor/final-services-mobile.jpg` | 375 × 1143 px | Narrow service cards |
| `work/design-monitor/final-monitor-mobile.jpg` | 375 × 1349 px | Narrow actual usage graphs |
| `work/design-monitor/deploy-mobile.jpg` | 390 × 844 px | Full-screen deployment sheet |
| `work/design-monitor/empty-monitor-desktop.jpg` | 1280 × 900 px | Empty project, no fabricated zero/history |

Desktop CSS viewport: 1280 × 900; narrow viewport: 390 × 844, density 1. Scrollbars reduce document content width to 1265/375 px in relevant captures. Dialog captures fill the full viewport. Browser captures are native JPEGs; no screenshot resampling or illustrative assets were used. Full-page mobile captures include the fixed navigation at the viewport's bottom, rather than at the full document's bottom.

## Findings and iteration history

The first rendered pass exposed these fixable issues:

- [P2] Long runner names hid distinguishing suffixes. Names now wrap and retain a full-name tooltip. The service drawer always shows the full identifier.
- [P2] A 100% minimum CPU scale made small real usage changes almost invisible. Aggregate CPU graphs now use a measured scale with explicit percentage ticks, while retaining the 100% = one logical CPU convention. Detail charts include their runtime limit.
- [P2] Switching to an empty workspace could retain a previous workspace's readout and accessible sample table. The empty path now clears readout, table, and keyboard metadata. A real empty project with valid ID `all` verified both history separation and absence of invented data.
- [P2] Editing while deployment preview was pending could leave Preview disabled. Edits now cancel the preview signature and restore Preview; real browser testing confirmed Preview enabled and Deploy disabled after an edit.
- [P2] Polling could restore detail-chart focus to a background sparkline. Restoration now uses the original dialog/main container and a focusable chart. Actual source samples advanced from 12 to 13 while keyboard focus and the selected older timestamp/value were retained. Evidence: `work/design-monitor/final-focus-after-poll.jpg`.
- [P2] Accessible sample tables grew the document invisibly after many real samples. They now sit inside clipped, fixed-size screen-reader wrappers instead of applying that clipping to a table with intrinsic row-height rules. This retains native table semantics without creating blank page space. Post-fix browser verification measured the same 1349 px document height at 2 and 29 actual samples; the wrapper remained 1 px high and native sample tables remained accessible. Revised evidence: `work/design-monitor/final-monitor-mobile.jpg`.
- [P3] Programmatic heading focus produced an input-like white rectangle. The heading keeps focus semantics with its own outline suppressed; interactive controls retain visible focus.

Source review also corrected scoped route parsing, target select synchronization, Deploy-here cancellation scope, deployment section anchors, and detail log identity when moving between services. A dependency-free helper check is saved as ignored evidence at `work/qa-dashboard.mjs`.

## Required visual surfaces

- **Fonts and typography:** native system sans with clear page/section/value hierarchy. Tabular numbers distinguish actual measurements from muted labels. Runner names wrap; technical paths and identifiers wrap in details. Compact table copy remains readable in focused captures.
- **Spacing and layout:** 224 px desktop navigation, compact sticky heading, moderate panel radii, consistent 16–24 px spacing, and right-side service/deployment sheets. Narrow screens use full-screen sheets, service cards, stacked graphs, and a fixed five-item navigation. Dense allocation/monitor tables scroll locally; the narrow document has no horizontal overflow.
- **Colors and tokens:** charcoal body `#17191b`, panels `#222528`, borders `#34383c`, text `#eceff1`, secondary text `#a9b0b7`. Real CPU history is muted blue and memory history muted purple. Green indicates running/live state. Default charcoal was confirmed as `rgb(23, 25, 27)` in browser.
- **Image quality and assets:** this interface requires no artwork. No placeholder images, fabricated diagrams, decorative icon system, slogans, or generated imagery were added. Canvas is used only for plots of actual observed samples.
- **Copy and content:** the interface distinguishes control-plane configuration, execution target, engine storage, group network, actual usage, reserved ceiling, effective limits, and global budget. Native observers remain monitor-only. Unknown telemetry is a dash or collecting state. Empty workspaces show no measured samples, rather than a zero trace.

## Interaction checks

- All five routed views and project scope navigation work, including valid project ID `all` without colliding with All workspaces.
- New workspace creation persists through the real Rust API.
- Deployment preview shows actual engine target, normalized configuration, project network, allocations, private empty volume, retained data semantics, and the non-root workspace permissions note.
- A cached node image deployed through the UI at 0.4 CPU / 256 MiB, with private `/workspace`, private project network, and loopback port 18888. The resulting service reported running, actual CPU/memory, engine PID, container ID, image/argv, mounts, network, and working directory.
- Deployment section links scroll within the sheet without changing the router URL. Edits invalidate review and disable Deploy. There is no invented multi-phase deployment progress.
- Real heartbeat logs loaded automatically. Graphs accumulated actual source timestamps, keyboard inspection worked, and accessible sample tables contained the same measurements. Graph history clears on reload and is bounded to one hour/720 samples.
- The pure helper checks cover syntax, duplicate/missing HTML IDs, workspace separation, null versus zero, partial coverage, measured timestamp ordering/deduplication, history bounds, stale preview cancellation, missing-interval trace separation, and empty-history metadata clearing.
- Search, service state filters, target filters, sorted monitor table, workspace selection, and global resource scope remain functional. Native observers have no lifecycle or resource mutation controls.
- There are no Refresh buttons. State and visible logs update automatically. During a real daemon restart, a stale/error banner retained previous data and retried automatically.
- Browser console output contained only the user's existing wallet extension warnings/errors; no application-source error was observed. A password manager injected an unrelated contact-save prompt after the test workspace name was filled; it was dismissed before final clean captures.

## Remaining limits

This pass does not certify every remote engine latency, image's filesystem permissions, or browser/screen-reader combination. Measurements describe monitored services across their actual execution hosts, not total host CPU or memory. The global pool controls managed Docker services and explicitly opted-in Linux systemd units. The existing Mac launchd services remain observers. Named groups have no separate budgets, and service history is session-only.

## Checklist

- [x] Implement Claude's functional charcoal redesign and workspace scope.
- [x] Show real execution locations, current usage, resource policy, and storage.
- [x] Deploy a real private-workspace service through preview/apply.
- [x] Verify automatic logs and actual CPU/memory history on desktop and narrow layouts.
- [x] Verify stale, empty, preview-invalidated, and read-only states.
- [x] Verify detail-chart keyboard focus and selected sample across an actual poll; confirm the heading has no input-like outline.
- [x] Verify final screen-reader table wrappers prevent invisible document growth at 29 real samples.
- [x] Save final comparison evidence and copy selected user-facing captures to the workspace outputs folder.
- [x] Restore the viewport and close only the temporary QA tab.

Browser QA used a temporary Chrome tab because this agent had no available in-app browser surface. Native browser navigation loaded localhost directly, retaining the server's cross-site request guard. All user-owned tabs were retained. The disposable worker was left running for final cleanup by the release workflow.
