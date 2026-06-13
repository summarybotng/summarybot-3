# SummaryBot — Product Documentation

SummaryBot turns the firehose of team conversation — WhatsApp, Discord, Slack —
into trustworthy, searchable knowledge: grounded summaries, rolling digests, and
an emergent knowledge base, delivered where your team already works.

This folder is the **product-level** documentation. For implementation detail see
the [ADRs](../adr/), the [PRD](../PRD.md), and the living
[coverage map](../coverage-map.md) (what is built vs. planned).

## Contents

| Doc | What it covers |
|-----|----------------|
| [Mission & Vision](mission-and-vision.md) | Why SummaryBot exists, what it's becoming, the principles that guide it |
| [Capabilities](capabilities.md) | What SummaryBot can do today (shipped + verified) |
| [Functional areas](functional-areas.md) | The subsystems — ingestion, summarization, scheduling, delivery, knowledge, tenancy, ops — and how they fit |
| [Roles & user stories](roles-and-user-stories.md) | Who uses SummaryBot, what they're allowed to do, and the jobs they come to get done |
| [User guide](user-guide.md) | Step-by-step: sign in, connect sources, summarize, schedule, deliver, curate, administer, deploy |

## One-paragraph orientation

SummaryBot is a **multi-tenant, multi-platform workspace platform** (not a
single-platform bot). A **tenant** is an organization; it contains **workspaces**;
each workspace draws messages from one or more **sources** (a WhatsApp export, a
Discord server, a Slack workspace). Messages are normalized, summarized by an LLM
into structured, **citation-grounded** summaries, and fanned out to **delivery
destinations** (the always-on dashboard plus optional sinks like a channel,
Confluence, email, or Google Drive). Every summary also feeds a per-workspace
**knowledge base** you can semantically search and synthesize into a wiki.

## Status legend

Throughout these docs, capabilities reflect the [coverage map](../coverage-map.md):
**shipped & tested** features are described in the present tense; anything still
planned is called out explicitly as *planned* or *roadmap*.
