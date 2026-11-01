# Dashboard design QA

final result: passed

## Comparison target and evidence

The user asked for a more comprehensive UI with charcoal colors, softer surfaces, and easier navigation. This supersedes the earlier light/olive direction. The full-screen before reference is `work/design-v2/dashboard-desktop-final.jpg` (1265 × 1815 px). The original rejected sidebar crop remains `work/design-v2/source-crop.png` (201 × 174 px); its decorative label, slogan, and version footer remain removed.

The before reference and the new rendered dashboard were opened together in the same comparison input. The wider scope intentionally adds resource information, persistent navigation, and service details. This is a scoped redesign, not a claim of pixel fidelity to the old colors or layout.

Implementation: `http://127.0.0.1:18787/`, using an isolated workspace with existing host services monitored read-only and two disposable managed Docker workers.

| Evidence | Pixel dimensions | State |
| --- | --- | --- |
| `work/design-v3-charcoal/dashboard-desktop.jpg` | 1265 × 2376 | Full dashboard, charcoal, all services |
| `work/design-v3-charcoal/resources-desktop.jpg` | 1265 × 712 | Expanded resource section |
| `work/design-v3-charcoal/service-overview-desktop.jpg` | 1265 × 712 | Managed service overview |
| `work/design-v3-charcoal/service-resources-desktop.jpg` | 1265 × 712 | Minimum, ceiling, effective, and runtime limits |
| `work/design-v3-charcoal/service-logs-desktop.jpg` | 1265 × 712 | Loaded real service logs |
| `work/design-v3-charcoal/service-activity-desktop.jpg` | 1265 × 712 | Service-specific controller events |
| `work/design-v3-charcoal/activity-filter-desktop.jpg` | 1265 × 712 | Activity filtered to Build queue |
| `work/design-v3-charcoal/job-overview-desktop.jpg` | 1265 × 712 | Disabled job with schedule and exit information |
| `work/design-v3-charcoal/dashboard-mobile.jpg` | 375 × 2320 | Narrow dashboard, worker search |
| `work/design-v3-charcoal/service-overview-mobile.jpg` | 375 × 812 | Narrow service details |
| `work/design-v3-charcoal/observer-resources-mobile.jpg` | 375 × 812 | Long service name, monitor-only resources |
| `work/design-v3-charcoal/service-sliders-mobile.jpg` | 375 × 812 | CPU and memory ceiling sliders |
| `work/design-v3-charcoal/allocation-applied-mobile.jpg` | 375 × 2391 | Successful live scaling and redistribution |
| `work/design-v3-charcoal/budget-preview-mobile.jpg` | 375 × 812 | Scrolled allocation preview, sticky title |
| `work/design-v3-charcoal/budget-validation-desktop.jpg` | 1265 × 712 | Rejected undersized budget, Apply disabled |

Desktop CSS viewport: 1280 × 720 at devicePixelRatio 1. Narrow CSS viewport: 390 × 844 at density 1. Native browser screenshots exclude scrollbars; document content widths are 1265 and 375 px. Browser captures use native JPEG output. No density resampling was used for comparison. Full-page screenshots were reviewed alongside focused dialogs because small table text needs a closer view than a long page permits.

## Findings

No actionable P0/P1/P2 findings remain in the reviewed browser interface.

- **Typography:** native system sans, clear page/section hierarchy, compact controls, and tabular numerical values. Long service names truncate in tables and remain available through titles and the details dialog. The long observer name wraps inside the narrow dialog without hiding Close.
- **Spacing and layout rhythm:** a persistent desktop sidebar separates navigation from the main workspace. Panels use moderate 8–12 px radii and consistent spacing. Narrow screens use a compact top navigation, stacked capacity information, and local horizontal scrolling for dense tables. The narrow document measured 375 px wide with a 375 px content viewport; there is no document overflow.
- **Colors and tokens:** the default body is charcoal `#17191b`, confirmed in-browser as `rgb(23, 25, 27)`. Panels use `#222528`, borders `#34383c`, primary text `#eceff1`, and secondary text `#a9b0b7`. Controls use neutral accents, with quiet green reserved for healthy/running state. A versioned theme preference makes charcoal the default independently of the earlier light preference.
- **Image quality and asset fidelity:** this infrastructure UI has no illustrative assets, logos, avatars, or icon artwork to reproduce. No placeholder art, generated decoration, custom SVG illustration, or fake icon was introduced. The rejected sidebar decoration stays removed.
- **Copy and content:** headings identify actual sections and actions. Host capacity, container capacity, and the workspace budget are distinct. Monitor-only usage is explicitly outside the pool. Resource views distinguish configured minimums, ceilings, effective allocation, and runtime-reported enforcement. Unknown telemetry uses “Not reported” or a dash; it is not fabricated.

## Interaction verification

- Sidebar links navigate to Services, Resources, and Activity. Active state and `aria-current` follow the visible section after scrolling.
- Service search, existing filters, and empty search results remain functional.
- Clicking a service name in either table opens its details dialog.
- Overview shows real state, runtime, control mode, service type, current usage, health, enabled state, and scheduling. Job details also show last/next run and last exit state.
- Overview/Resources/Logs/Activity tabs work with pointer input and ArrowLeft/ArrowRight keyboard navigation, including visible focus and selected state.
- Resource details show actual runtime limits alongside policy and effective values. The details-to-Edit-limits flow opens the existing slider dialog.
- Real logs loaded in the service Logs tab. Service Activity contains matching controller events; the global service filter showed only Build queue events when selected.
- Existing observed services retain disabled lifecycle and scaling controls in both tables and service details. The disabled Renovate job remains disabled.
- On a disposable worker, ArrowRight synchronized the CPU slider and number input from 0.20 to 0.21 cores. Applying the ceiling succeeded, and the other worker's effective share changed from 0.40 to 0.39 cores within the unchanged 0.60-core pool. Memory remained 128/256 MiB within the 384-MiB pool.
- An automatic budget of 0.01 cores / 6 MiB was visibly rejected because service minimums cannot fit. Apply stayed disabled, and the persisted budget was unchanged.
- Budget preview, sticky dialog title, dismissal, loading states, disabled submission during apply, narrow layout, and readable charcoal controls were checked.
- Browser console warning/error checks returned an empty list. JavaScript syntax checking with Node's VM and whitespace checks passed.
- The temporary preview tab was closed after testing. The user's original dashboard tab was retained. The normal viewport was restored, and the preview services were left running for final backend checks.

## Comparison history

1. The initial light refinement removed the rejected decorative sidebar and added native resource sliders. A long dialog's title could scroll away; the sticky header fix was implemented and verified in `work/design-v2/resource-budget-desktop-final.jpg` and its narrow dark counterpart. Sequenced log requests prevent an older response from appearing under a newly opened service title.
2. The user then requested charcoal, broader information, and easier navigation. The current implementation intentionally changes palette and layout, adds real detail/resource/activity views, and retains those fixes. The current browser captures show no new actionable visual or interaction issues; no post-QA asset edits were required.

## Implementation checklist

- [x] Apply the charcoal default and softer surfaces.
- [x] Add functional persistent section navigation and workspace identity.
- [x] Add real service details, resource policy/enforcement views, logs, and activity filtering.
- [x] Retain real CPU/memory sliders, preview validation, and Rust-controlled allocation.
- [x] Verify desktop and narrow layouts, keyboard detail tabs, observer protections, and live scaling.
- [x] Save browser evidence, inspect console errors, and compare source/implementation captures.

## Follow-up polish and limits

No P3 visual change is required for this scope. Telemetry sampling and host supervisors can add seconds to state and mutation requests; a prior rebuilt state request measured 4.089 seconds while preview/polling overlapped. This pass does not certify every remote-host latency condition or every screen-reader/browser combination. Native macOS services remain monitor-only because the host integration cannot enforce per-service hard CPU/memory limits.
