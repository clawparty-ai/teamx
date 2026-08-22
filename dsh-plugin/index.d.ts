/**
 * teamx deepseek-harness plugin.
 *
 * Registers teamx_* tools, /team-* slash commands, per-agent digest injection,
 * and real-time WebSocket push. Loaded by dsh as a Cordis plugin entry.
 *
 * @module @teamx-ai/dsh-plugin
 */

import type { Context } from '@deepseek-ai/cordis'

export interface Config {
  /** Team name to auto-join on startup (optional). */
  team?: string
  /** Polling interval in ms for digest refresh (default: 15000; 0 disables). */
  pollIntervalMs?: number
  /** Enable WebSocket push (default: true when TEAMX_SERVER_URL is set). */
  wsEnabled?: boolean
}

/** Mount the teamx collaboration plugin into a dsh Cordis context. */
export declare function apply(ctx: Context, config?: Config): void
