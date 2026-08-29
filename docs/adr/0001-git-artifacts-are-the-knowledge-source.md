# Git artifacts are the knowledge source

Teamx keeps durable team knowledge such as the glossary, architecture decisions, and design documents as versioned files in the Git workspace. The Teamx ledger records coordination events and references to those artifacts, rather than duplicating their content in SQLite, because Git already provides the review, history, and merge semantics that collaborative documents require.
