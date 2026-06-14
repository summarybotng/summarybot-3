# v2 (summarybot-ng) ADRs — reference spec

The committed, **first-class** architecture-decision record of the legacy
**summarybot-ng** product, copied verbatim from the reference repo
(`/workspaces/summarybot-ng-reference/docs/adr`) so the rewrite is designed
**against the spec**, not reverse-engineered from whatever Rust happens to exist.

> **Status: reference, not rewrite truth.** These describe v2. Re-derive for
> Rust; where the rewrite intentionally diverges, that delta is recorded in this
> repo's own ADRs (`docs/adr/`, ADR-119+) and the [coverage map](../../coverage-map.md).
> When a rewrite feature is unclear, the relevant v2 ADR here is the source of truth
> for intended behavior (see [conventions/v2-adrs-are-the-spec](../../conventions/v2-adrs-are-the-spec.md)).

## Index

- [`001-whatsapp-conversation-reader-bot.md`](001-whatsapp-conversation-reader-bot.md) — ADR-001: WhatsApp Conversation Reader Bot
- [`002-whatsapp-datasource-integration-summarybotng.md`](002-whatsapp-datasource-integration-summarybotng.md) — ADR-002: WhatsApp Data Source Integration with SummaryBot-NG
- [`003-summarybotng-modifications-for-whatsapp.md`](003-summarybotng-modifications-for-whatsapp.md) — ADR-003: Concrete SummaryBot-NG Modifications for WhatsApp Integration
- [`004-grounded-summary-references.md`](004-grounded-summary-references.md) — ADR-004: Grounded Summary References — Citing Conversation Sources
- [`005-summary-delivery-destinations.md`](005-summary-delivery-destinations.md) — ADR-005: Summary Delivery Destinations — UI Storage and Channel Push
- [`006-retrospective-summary-archive.md`](006-retrospective-summary-archive.md) — ADR-006: Retrospective Summary Archive — Historical Backfill with Versioned Prompts
- [`007-per-server-google-drive-sync.md`](007-per-server-google-drive-sync.md) — ADR-007: Per-Server Google Drive Sync with Fallback
- [`008-unified-summary-experience.md`](008-unified-summary-experience.md) — ADR-008: Unified Summary Experience — Archive and Real-time Parity
- [`009-schedule-run-summary-navigation.md`](009-schedule-run-summary-navigation.md) — ADR-009: Schedule → Run → Summary Navigation
- [`010-prompt-repository-navigation.md`](010-prompt-repository-navigation.md) — ADR-010: Prompt Repository Navigation
- [`011-unified-scope-selection.md`](011-unified-scope-selection.md) — ADR-011: Unified Scope Selection for All Summary Types
- [`012-summaries-ui-consolidation.md`](012-summaries-ui-consolidation.md) — ADR-012: Summaries UI Consolidation
- [`013-unified-job-tracking.md`](013-unified-job-tracking.md) — ADR-013: Unified Job Tracking
- [`014-discord-push-templates.md`](014-discord-push-templates.md) — ADR-014: Discord Push Templates with Thread Support
- [`015-summary-deep-linking.md`](015-summary-deep-linking.md) — ADR-015: Summary Deep Linking and Search
- [`016-summary-regeneration-data-integrity.md`](016-summary-regeneration-data-integrity.md) — ADR-016: Summary Regeneration Data Integrity
- [`017-summary-overview-navigation.md`](017-summary-overview-navigation.md) — ADR-017: Summary Overview and Navigation
- [`018-bulk-summary-operations.md`](018-bulk-summary-operations.md) — ADR-018: Bulk Summary Operations
- [`019-database-primary-archive-storage.md`](019-database-primary-archive-storage.md) — ADR-019: Database-Primary Archive Storage
- [`020-summary-navigation-and-search.md`](020-summary-navigation-and-search.md) — ADR-020: Summary Navigation and Search
- [`021-content-count-filters.md`](021-content-count-filters.md) — ADR-021: Content Count Filters
- [`022-auto-retry-truncated-responses.md`](022-auto-retry-truncated-responses.md) — ADR-022: Auto-Retry Truncated LLM Responses
- [`023-json-parse-error-handling.md`](023-json-parse-error-handling.md) — ADR-023: Handling Invalid JSON Responses from LLM
- [`024-resilient-summary-generation.md`](024-resilient-summary-generation.md) — ADR-024: Resilient Summary Generation with Multi-Model Retry
- [`024-service-resilience.md`](024-service-resilience.md) — ADR-024: Service Resilience and Availability
- [`025-resilient-summary-generation.md`](025-resilient-summary-generation.md) — ADR-025: Resilient Summary Generation with Multi-Model Retry
- [`026-multi-platform-source-architecture.md`](026-multi-platform-source-architecture.md) — ADR-026: Multi-Platform Source Architecture
- [`027-retrospective-coverage-view.md`](027-retrospective-coverage-view.md) — ADR-027: Retrospective Coverage View
- [`028-whatsapp-pii-anonymization.md`](028-whatsapp-pii-anonymization.md) — ADR-028: WhatsApp PII Anonymization
- [`029-confluence-integration.md`](029-confluence-integration.md) — ADR-029: Confluence Integration
- [`030-email-delivery-destination.md`](030-email-delivery-destination.md) — ADR-030: Email Delivery Destination
- [`031-comprehensive-error-logging.md`](031-comprehensive-error-logging.md) — ADR-031: Comprehensive Error Logging
- [`032-email-content-and-landing-pages.md`](032-email-content-and-landing-pages.md) — ADR-032: Email Content Templates and Landing Pages
- [`033-custom-perspectives.md`](033-custom-perspectives.md) — ADR-033: Custom Perspectives
- [`034-guild-prompt-templates.md`](034-guild-prompt-templates.md) — ADR-034: Guild Prompt Templates
- [`035-summary-date-selection-and-filters.md`](035-summary-date-selection-and-filters.md) — ADR-035: Summary Date Selection and Generation Filters
- [`036-timezone-handling.md`](036-timezone-handling.md) — ADR-036: Consistent Timezone Handling
- [`037-centralized-filter-criteria.md`](037-centralized-filter-criteria.md) — ADR-037: Centralized Filter Criteria for Lists and Feeds
- [`ADR-038-self-healing-parameter-validation.md`](ADR-038-self-healing-parameter-validation.md) — ADR-038: Self-Healing Parameter Validation
- [`ADR-039-user-problem-reporting.md`](ADR-039-user-problem-reporting.md) — ADR-039: User Problem Reporting System
- [`ADR-040-jobs-navigation-rethink.md`](ADR-040-jobs-navigation-rethink.md) — ADR-040: Jobs Navigation Rethink
- [`ADR-041-soft-fail-channel-permissions.md`](ADR-041-soft-fail-channel-permissions.md) — ADR-041: Soft-Fail Channel Permission Handling
- [`ADR-042-intelligent-job-retry.md`](ADR-042-intelligent-job-retry.md) — ADR-042: Intelligent Job Retry Strategy
- [`ADR-043-slack-workspace-integration-feasibility.md`](ADR-043-slack-workspace-integration-feasibility.md) — ADR-043: Slack Workspace Integration — Feasibility Study
- [`ADR-044-deferred-technical-debt-tracker.md`](ADR-044-deferred-technical-debt-tracker.md) — ADR-044: Deferred Technical Debt Tracker
- [`ADR-045-audit-logging-system.md`](ADR-045-audit-logging-system.md) — ADR-045: Audit Logging System
- [`ADR-046-channel-permission-aware-summaries.md`](ADR-046-channel-permission-aware-summaries.md) — ADR-046: Channel Permission-Aware Summary Visibility
- [`ADR-047-discord-dm-delivery.md`](ADR-047-discord-dm-delivery.md) — ADR-047: Discord Direct Message Delivery Destination
- [`ADR-048-insufficient-content-skip.md`](ADR-048-insufficient-content-skip.md) — ADR-048: Insufficient Content Handling - Skip vs Fail
- [`ADR-049-google-workspace-sso.md`](ADR-049-google-workspace-sso.md) — ADR-049: Google Workspace SSO with Domain Restriction
- [`ADR-050-google-workspace-group-admin.md`](ADR-050-google-workspace-group-admin.md) — ADR-050: Google Workspace Group-Based Admin Access
- [`ADR-051-platform-message-fetcher-abstraction.md`](ADR-051-platform-message-fetcher-abstraction.md) — ADR-051: Platform Message Fetcher Abstraction
- [`ADR-052-ruvector-integration-vision.md`](ADR-052-ruvector-integration-vision.md) — ADR-052: RuVector Integration Vision
- [`ADR-053-whatsapp-live-fetch-feasibility.md`](ADR-053-whatsapp-live-fetch-feasibility.md) — ADR-053: WhatsApp Live Fetch Feasibility Assessment
- [`ADR-054-operational-agents.md`](ADR-054-operational-agents.md) — ADR-054: Operational Agents for Instance Management
- [`ADR-055-knowledge-base-agents.md`](ADR-055-knowledge-base-agents.md) — ADR-055: Knowledge Base Agents - Enhancement Layer
- [`ADR-056-compounding-wiki-standard.md`](ADR-056-compounding-wiki-standard.md) — ADR-056: Compounding Wiki - Standard Implementation
- [`ADR-057-compounding-wiki-ruvector.md`](ADR-057-compounding-wiki-ruvector.md) — ADR-057: Compounding Wiki - RuVector Enhanced Implementation
- [`ADR-058-wiki-rendering.md`](ADR-058-wiki-rendering.md) — ADR-058: Wiki Rendering
- [`ADR-059-wiki-external-sync.md`](ADR-059-wiki-external-sync.md) — ADR-059: Wiki External Sync (Google Drive)
- [`ADR-060-wiki-curation-model.md`](ADR-060-wiki-curation-model.md) — ADR-060: Wiki Curation Model - Human + AI Collaboration
- [`ADR-061-wiki-population-strategies.md`](ADR-061-wiki-population-strategies.md) — ADR-061: Wiki Population Strategies
- [`ADR-062-summary-repository-alignment.md`](ADR-062-summary-repository-alignment.md) — ADR-062: Summary Repository Architecture Alignment
- [`ADR-063-wiki-page-tabs.md`](ADR-063-wiki-page-tabs.md) — ADR-063: Wiki Page Tabs (Updates + Synthesis)
- [`ADR-064-wiki-navigation-filters.md`](ADR-064-wiki-navigation-filters.md) — ADR-064: Wiki Navigation Filters
- [`ADR-065-wiki-synthesis-controls.md`](ADR-065-wiki-synthesis-controls.md) — ADR-065: Wiki Synthesis Rating & Regeneration Controls
- [`ADR-066-platform-agnostic-architecture.md`](ADR-066-platform-agnostic-architecture.md) — ADR-066: Platform-Agnostic Architecture
- [`ADR-067-automatic-wiki-ingestion.md`](ADR-067-automatic-wiki-ingestion.md) — ADR-067: Automatic Wiki Ingestion
- [`ADR-068-wiki-backfill-jobs.md`](ADR-068-wiki-backfill-jobs.md) — ADR-068: Wiki Backfill Jobs
- [`ADR-069-wiki-and-jobs-ux-improvements.md`](ADR-069-wiki-and-jobs-ux-improvements.md) — ADR-069: Wiki and Jobs UX Improvements
- [`ADR-070-public-issue-tracker.md`](ADR-070-public-issue-tracker.md) — ADR-070: Public Issue Tracker
- [`ADR-071-summary-deduplication.md`](ADR-071-summary-deduplication.md) — ADR-071: Summary Deduplication Strategy
- [`ADR-072-content-coverage-tracking.md`](ADR-072-content-coverage-tracking.md) — ADR-072: Content Coverage Tracking and Scheduled Backfill
- [`ADR-073-channel-access-controls.md`](ADR-073-channel-access-controls.md) — ADR-073: Channel Access Controls and Summary Governance
- [`ADR-074-private-channel-content-detection.md`](ADR-074-private-channel-content-detection.md) — ADR-074: Private Channel Content Detection
- [`ADR-075-private-content-regeneration-split.md`](ADR-075-private-content-regeneration-split.md) — ADR-075: Private Content Regeneration Split
- [`ADR-076-continuous-wiki-synthesis.md`](ADR-076-continuous-wiki-synthesis.md) — ADR-076: Continuous Wiki Synthesis
- [`ADR-077-ai-wiki-curator-agent.md`](ADR-077-ai-wiki-curator-agent.md) — ADR-077: AI Wiki Curator Agent
- [`ADR-078-platform-agnostic-ux.md`](ADR-078-platform-agnostic-ux.md) — ADR-078: Platform-Agnostic UX Design
- [`ADR-079-subdomain-multi-tenancy.md`](ADR-079-subdomain-multi-tenancy.md) — ADR-079: Subdomain Multi-Tenancy
- [`ADR-080-wiki-perspective-filtering.md`](ADR-080-wiki-perspective-filtering.md) — ADR-080: Wiki Perspective Filtering
- [`ADR-081-whatsapp-import-management.md`](ADR-081-whatsapp-import-management.md) — ADR-081: WhatsApp Import Management
- [`ADR-082-google-drive-import.md`](ADR-082-google-drive-import.md) — ADR-082: Google Drive Import for WhatsApp Exports
- [`ADR-083-whatsapp-manual-summarization.md`](ADR-083-whatsapp-manual-summarization.md) — ADR-083: WhatsApp Manual Summarization
- [`ADR-084-bulk-wiki-regeneration.md`](ADR-084-bulk-wiki-regeneration.md) — ADR-084: Bulk Wiki Regeneration
- [`ADR-085-source-guild-relationships.md`](ADR-085-source-guild-relationships.md) — ADR-085: Source-Guild Relationship Model
- [`ADR-086-summary-wiki-bidirectional-navigation.md`](ADR-086-summary-wiki-bidirectional-navigation.md) — ADR-086: Bidirectional Summary-Wiki Navigation
- [`ADR-087-wiki-ingestion-granularity.md`](ADR-087-wiki-ingestion-granularity.md) — ADR-087: Wiki Ingestion Granularity - Cross-Channel vs. Temporal Strategies
- [`ADR-088-unified-scheduling-ux.md`](ADR-088-unified-scheduling-ux.md) — ADR-088: Unified Multi-Platform Scheduling UX
- [`ADR-089-simplified-scheduling-ux.md`](ADR-089-simplified-scheduling-ux.md) — ADR-089: Unified Summary Creation UX
- [`ADR-090-ruvector-emergent-wiki-structure.md`](ADR-090-ruvector-emergent-wiki-structure.md) — ADR-090: RuVector Emergent Wiki Structure
- [`ADR-091-sync-export-configuration.md`](ADR-091-sync-export-configuration.md) — ADR-091: Google Drive Sync Export Configuration
- [`ADR-092-ruvector-explorer-page.md`](ADR-092-ruvector-explorer-page.md) — ADR-092: RuVector Explorer Dashboard
- [`ADR-093-ruvector-knowledge-graph.md`](ADR-093-ruvector-knowledge-graph.md) — ADR-093: RuVector Knowledge Graph Visualization
- [`ADR-094-summary-split-mode.md`](ADR-094-summary-split-mode.md) — ADR-094: Summary Split Mode for Multi-Channel Summaries
- [`ADR-095-adaptive-token-allocation.md`](ADR-095-adaptive-token-allocation.md) — ADR-095: Adaptive Token Allocation for Summarization
- [`ADR-096-adaptive-summary-granularity.md`](ADR-096-adaptive-summary-granularity.md) — ADR-096: Adaptive Summary Granularity
- [`ADR-097-channel-accessibility-display.md`](ADR-097-channel-accessibility-display.md) — ADR-097: Channel Accessibility Display
- [`ADR-098-summary-scope-metadata.md`](ADR-098-summary-scope-metadata.md) — ADR-098: Summary Scope Metadata and Filtering
- [`ADR-099-remote-platform-publishing.md`](ADR-099-remote-platform-publishing.md) — ADR-099: Remote Platform Publishing
- [`ADR-100-confluence-content-enrichment.md`](ADR-100-confluence-content-enrichment.md) — ADR-100: Confluence Content Enrichment
- [`ADR-101-rolling-period-summaries.md`](ADR-101-rolling-period-summaries.md) — ADR-101: Rolling Period Summaries
- [`ADR-102-schedule-summary-job-traceability.md`](ADR-102-schedule-summary-job-traceability.md) — ADR-102: Schedule-Summary-Job Traceability
- [`ADR-103-schedule-attribution-filtering.md`](ADR-103-schedule-attribution-filtering.md) — ADR-103: Schedule Attribution and Filtering for Summaries
- [`ADR-104-rolling-schedule-summary-display.md`](ADR-104-rolling-schedule-summary-display.md) — ADR-104: Rolling Schedule Summary Display
- [`ADR-105-frequency-rolling-period-constraints.md`](ADR-105-frequency-rolling-period-constraints.md) — ADR-105-frequency-rolling-period-constraints.md
- [`ADR-106-summary-metadata-panel-redesign.md`](ADR-106-summary-metadata-panel-redesign.md) — ADR-106: Summary Metadata Panel Redesign
- [`ADR-107-smart-lookback-defaults.md`](ADR-107-smart-lookback-defaults.md) — ADR-107: Smart Lookback Period Defaults
- [`ADR-108-rolling-delivery-per-destination.md`](ADR-108-rolling-delivery-per-destination.md) — ADR-108: Per-Destination Rolling Period Delivery Control
- [`ADR-109-bi-directional-schedule-summary-linking.md`](ADR-109-bi-directional-schedule-summary-linking.md) — ADR-109: Bi-Directional Schedule-Summary Linking
- [`ADR-110-bulk-confluence-publish-unpublish.md`](ADR-110-bulk-confluence-publish-unpublish.md) — ADR-110: Bulk Confluence Publish/Unpublish
- [`ADR-111-retrospective-confluence-auto-publish.md`](ADR-111-retrospective-confluence-auto-publish.md) — ADR-111: Retrospective Summary Auto-Publish to Confluence
- [`ADR-112-whatsapp-coverage-gap-awareness.md`](ADR-112-whatsapp-coverage-gap-awareness.md) — ADR-112: WhatsApp Coverage Gap Awareness
- [`ADR-114-confluence-metadata-properties.md`](ADR-114-confluence-metadata-properties.md) — ADR-114: Confluence Metadata and Page Properties
- [`ADR-117-ruvector-rvf-export.md`](ADR-117-ruvector-rvf-export.md) — ADR-117: RuVector RVF File Export
- [`ADR-118-ruvector-deduplication.md`](ADR-118-ruvector-deduplication.md) — ADR-118: RuVector Deduplication During Rolling Schedule Updates
