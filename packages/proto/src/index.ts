// The IPC protocol, as TypeScript.
//
// `./bindings` is written by `cargo test -p vortex-proto` from the Rust definitions in
// `crates/vortex-proto`. Nothing in there is edited by hand — if a shape looks wrong, the
// Rust type is wrong. This barrel is the only hand-written file, and a test in
// `tests/index.test.ts` fails if it ever falls behind the generated directory.

export { MAX_FRAME, PROTOCOL_VERSION } from "./bindings/constants";
export { bytes, duration, estimate, eta, percent, ratio, rate } from "./format";

export type { Category } from "./bindings/Category";
export type { Command } from "./bindings/Command";
export type { ContainerPreference } from "./bindings/ContainerPreference";
export type { Decision } from "./bindings/Decision";
export type { Event } from "./bindings/Event";
export type { JobId } from "./bindings/JobId";
export type { JobSpec } from "./bindings/JobSpec";
export type { JobState } from "./bindings/JobState";
export type { JobView } from "./bindings/JobView";
export type { MediaCandidate } from "./bindings/MediaCandidate";
export type { MediaKind } from "./bindings/MediaKind";
export type { MediaSelection } from "./bindings/MediaSelection";
export type { MediaSummary } from "./bindings/MediaSummary";
export type { MediaTrack } from "./bindings/MediaTrack";
export type { MediaVariant } from "./bindings/MediaVariant";
export type { Outcome } from "./bindings/Outcome";
export type { Priority } from "./bindings/Priority";
export type { ProbeResult } from "./bindings/ProbeResult";
export type { ProgressFrame } from "./bindings/ProgressFrame";
export type { Protocol } from "./bindings/Protocol";
export type { RenewalHint } from "./bindings/RenewalHint";
export type { RequestEnvelope } from "./bindings/RequestEnvelope";
export type { Resolution } from "./bindings/Resolution";
export type { Retries } from "./bindings/Retries";
export type { Settings } from "./bindings/Settings";
export type { SubscriptionScope } from "./bindings/SubscriptionScope";
export type { SummaryFrame } from "./bindings/SummaryFrame";
export type { TabId } from "./bindings/TabId";
export type { Theme } from "./bindings/Theme";
export type { TransferMode } from "./bindings/TransferMode";
export type { Verification } from "./bindings/Verification";
export type { WorkerFrame } from "./bindings/WorkerFrame";
