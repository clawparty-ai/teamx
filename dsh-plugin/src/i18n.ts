/**
 * Message strings for teamx-dsh-plugin.
 * @module @teamx/dsh-plugin/i18n
 */

export const M = {
  // Tool messages
  NO_SESSION: 'No session available. Are you in an agent context?',
  TEAM_CREATED: 'Team created',
  JOINED: 'Joined team',
  LEFT: 'Left team',
  APPROVED: 'Member approved',
  DENIED: 'Member denied',
  GOAL_SET: 'Goal set',
  GOAL_SHARED: 'Goal shared',
  GOAL_CLOSED: 'Goal closed',
  INVITE_SENT: 'Invite sent',
  PUBLISHED: 'Event published',
  ROLE_SET: 'Role set',
  STATE_SET: 'State set',
  ASKED: 'Question sent',
  RESPONDED: 'Question answered',
  SYNCED: 'Synced',
  TEAMS_LISTED: 'Teams listed',
  STATUS_SHOWN: 'Status shown',
  ARCHIVED: 'Team archived',
  DESTROYED: 'Team destroyed',

  // Auto-execute
  AUTO_EXECUTE_PREFIX: '⚡ AUTO-EXECUTE:',
  SYNC_PROMPT: 'Sync your state before proceeding.',
  DIRECTED_TASK: 'You have been assigned a task:',

  // Errors
  ERROR_NO_TEAM: 'No team found. Create or join a team first.',
  ERROR_NOT_OWNER: 'Only the team owner can perform this action.',
  ERROR_NOT_FOUND: 'Not found.',
  ERROR_BINARY: 'teamx binary error.',
  ERROR_NETWORK: 'Network error connecting to teamx server.',

  // Digest
  DIGEST_HEADER: '📋 Team status:',
  DIGEST_GOAL: '🎯 Goal:',
  DIGEST_MEMBERS: '👥 Members:',
  DIGEST_EVENTS: '📝 Recent:',
} as const
