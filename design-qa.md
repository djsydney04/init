# Init design QA

final result: passed

No actionable P0/P1/P2 visual findings remain in the reviewed states.

## Design source and comparison

Claude Opus 5.5 reviewed the six user-provided references and the existing interface through Cursor CLI (`claude-opus-5-5-high`). Its design specification is retained locally at `work/opus-init/design.md`. Docker Desktop informs service controls and navigation; the dashboard references inform light surfaces, restrained green accents, dark mode, and prominent charts. This is a service-specific redesign, not a pixel copy of any reference.

The reference images and rendered implementation were opened together for comparison. The final comparison included reference 6, the complete desktop overview, and the corrected deployment review. Focused inspection covered the review footer, mobile port fields, service controls, and chart labels. The implementation intentionally uses actual measurements from one disposable worker; reference service counts and chart shapes are not replicated with fabricated data.

Browser QA used the real Rust server at `http://127.0.0.1:18789`, an isolated manifest, and an owned Docker worker. The user's existing controller and services were not modified. The desktop CSS viewport was 1440 × 1000; the mobile viewport was 390 × 844. Scrollbars reduce the available document width to 1425/375 px in some states. Native browser captures are retained locally under `work/qa-init/`; long mobile captures place the fixed navigation at the viewport bottom.

## Evidence

| Local capture | Reviewed state |
| --- | --- |
| `overview-desktop-final.png` | Light overview, real CPU/memory history, status and execution target |
| `overview-dark.png` | Dark theme with readable labels and graph contrast |
| `overview-mobile.png` | Stacked actual charts and mobile navigation |
| `services-mobile.png` | Service controls, live usage, filtering and local target |
| `deploy-review-final.png` | Validated deployment and fully visible footer |
| `deploy-mobile.png` | Mobile resource step with persistent action buttons |
| `ports-mobile.png` | Structured computer/service ports and protocol |
| `github-desktop-final.png` | Workflow generator and setup instructions |
| `github-mobile.png` | Stacked setup form and locally scrolling workflow |
| `empty-overview.png` | Empty workspace with no inherited or invented measurements |

## Findings resolved

- **P2 — deployment footer clipped:** the review dialog's sticky footer overlapped its own bottom margin. Removing the negative bottom offset/margin keeps Cancel, Back and Deploy visible. Rechecked with a complete preview on desktop and a long resource form on mobile; mobile buttons end at 828 px within an 844 px viewport.
- **P2 — empty coverage message:** zero active services could appear as partial measurement coverage. Empty workspaces now say “No active services,” clear prior values and sample tables, and draw no fabricated zero trace.
- **P3 — healthy attention indicator:** zero services needing attention used an error-colored dot. It now uses a neutral indicator and “None need attention.”

One early overview capture and a cropped browser capture were incomplete render artifacts. They were discarded from the evidence list; the final viewport captures above show the settled interface.

## Visual checks

- Native system typography provides distinct page, section, value and helper-text hierarchy; tabular numbers make changing measurements legible.
- A 232 px desktop sidebar keeps navigation and Deploy service visible. Mobile uses four bottom controls plus a More menu; forms become full-screen and charts stack.
- White and muted gray light surfaces, restrained green actions/CPU plots, and blue memory plots follow the approved direction. Dark mode preserves the same hierarchy.
- Phosphor icons are copied from the official package with their MIT notice in `assets/PHOSPHOR-LICENSE`; no external icon/font request is required.
- No page-level horizontal overflow was observed in the reviewed mobile Overview, GitHub or Resources views. Workflow text and allocation tables scroll locally. Long target names and paths wrap where needed.
- Charts use only actual timestamps and values, preserve missing intervals, expose sample tables and keyboard selection, and explicitly state that history clears on reload and covers monitored services rather than the entire host.

## Interaction and runtime checks

- All six views navigate correctly. Existing `monitor` and `events` routes resolve to Overview and Activity while preserving workspace scope.
- Desktop and mobile theme controls work. The More menu opens Resources. Workspace creation persists, and selecting a new empty workspace clears previous chart values and sample history.
- Keyboard chart focus and the selected older sample survived a real polling update.
- Deployment presets populate image, limits and structured ports; Undo restores the prior draft. Preview validated `127.0.0.1:18080:80`; changing the computer port invalidated the reviewed preview and disabled Deploy.
- The UI deployed a real worker with private storage. Its details, resource measurements and heartbeat logs loaded from the configured engine.
- GitHub setup generated and downloaded a workflow with the chosen service, branch, runner and local paths. Contract checks execute its shell through a stub, proving paths containing spaces, quotes and shell syntax remain literal arguments. Invalid GitHub expressions and unsafe field values are rejected.
- Real image replacement built through the configured engine, retained hard limits and volume data, and restored the exact original container and manifest after a bad replacement failed startup. Evidence: `runtime-validation.txt`.
- Core/CLI tests (66), desktop tests, both Clippy checks, service sorting checks, UI contracts and workflow contracts passed. Browser diagnostics reported no warnings or errors.

## Scope and remaining limits

The GitHub workflow is generated for review and commit; this pass did not register a runner, connect a GitHub account or execute a production GitHub Actions deployment. A VM engine must have access to the runner's checkout path. Image replacement briefly stops the service and checks startup for one second; it does not certify application health or reverse shared-volume writes/migrations. SDK publication and installer signing were not performed. These constraints are documented in `docs/github-deployments.md` and surfaced where relevant in the UI.
