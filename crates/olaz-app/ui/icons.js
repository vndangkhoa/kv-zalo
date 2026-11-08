// Authored 24x24 inline SVG icon system for Olaz Linux.
// Consistent 1.75px stroke, round caps & joins, zero emoji or Unicode icon glyphs.

const PATHS = {
  olazLogo: `
    <path d="M5 6.5A2.5 2.5 0 0 1 7.5 4h9A2.5 2.5 0 0 1 19 6.5v7A2.5 2.5 0 0 1 16.5 16H11l-4.2 3.4c-.5.4-1.3.05-1.3-.6V16A2.5 2.5 0 0 1 5 13.5v-7Z" />
    <path d="M9 9.25h6M9 12.25h3.75" />
  `,
  inbox: `
    <path d="M4 6.5A2.5 2.5 0 0 1 6.5 4h11A2.5 2.5 0 0 1 20 6.5v8A2.5 2.5 0 0 1 17.5 17H11l-4.2 3.2c-.5.38-1.3.02-1.3-.62V17A2.5 2.5 0 0 1 4 14.5v-8Z" />
  `,
  unread: `
    <path d="M19.5 10.5v4A2.5 2.5 0 0 1 17 17h-6l-4 3v-3H6.5A2.5 2.5 0 0 1 4 14.5v-8A2.5 2.5 0 0 1 6.5 4h7" />
    <circle cx="18" cy="6" r="2.75" />
  `,
  other: `
    <rect x="3.5" y="4.5" width="17" height="4.5" rx="1.25" />
    <path d="M5 9v8.5A2 2 0 0 0 7 19.5h10a2 2 0 0 0 2-2V9" />
    <path d="M10 13.25h4" />
  `,
  strangers: `
    <circle cx="10" cy="8" r="3.25" />
    <path d="M4.25 19a5.75 5.75 0 0 1 11.5 0" />
    <path d="M18 8.25a2 2 0 0 1 1.5 3.3c-.5.48-.9.82-.9 1.45" />
    <circle cx="18.6" cy="16" r="0.6" fill="currentColor" stroke="none" />
  `,
  contacts: `
    <circle cx="9.25" cy="8.25" r="3.25" />
    <path d="M3.75 19a5.5 5.5 0 0 1 11 0" />
    <path d="M15.5 5.5a3 3 0 0 1 0 5.5M17.25 13.5A4.75 4.75 0 0 1 20.5 19" />
  `,
  usersGroup: `
    <circle cx="12" cy="7.75" r="2.75" />
    <path d="M7.25 19a4.75 4.75 0 0 1 9.5 0" />
    <circle cx="5.5" cy="9.5" r="2" />
    <path d="M2.25 18.5a3.5 3.5 0 0 1 3.5-3.5" />
    <circle cx="18.5" cy="9.5" r="2" />
    <path d="M21.75 18.5a3.5 3.5 0 0 0-3.5-3.5" />
  `,
  userPlus: `
    <circle cx="9.5" cy="8.25" r="3.25" />
    <path d="M4 19a5.5 5.5 0 0 1 11 0" />
    <path d="M18.25 8.25v5.5M15.5 11h5.5" />
  `,
  cloud: `
    <path d="M6.75 18.5h10.75a4 4 0 0 0 .6-7.95A5.5 5.5 0 0 0 7.3 9.1 3.75 3.75 0 0 0 6.75 18.5Z" />
  `,
  zcloud: `
    <path d="M6.5 16.5h11a3.5 3.5 0 0 0 .5-6.96A5 5 0 0 0 7.2 8.2a3.25 3.25 0 0 0-.7 8.3Z" />
    <path d="M4.5 20h15" />
  `,
  lock: `
    <rect x="5" y="10.5" width="14" height="9.5" rx="2" />
    <path d="M8 10.5V7.75a4 4 0 0 1 8 0v2.75" />
    <path d="M12 14.5v2" />
  `,
  unlock: `
    <rect x="5" y="10.5" width="14" height="9.5" rx="2" />
    <path d="M8 10.5V7.75a4 4 0 0 1 7.6-1.75" />
  `,
  shield: `
    <path d="M12 3.5 5.25 6.25v5.3c0 4.35 2.88 7.86 6.75 9.2 3.87-1.34 6.75-4.85 6.75-9.2v-5.3L12 3.5Z" />
    <path d="m9.5 12 1.75 1.75 3.5-3.5" />
  `,
  pin: `
    <path d="m14.5 4.5 5 5-2.6 1.2-2.65 2.65.5 3.65-1.5 1.5-3.75-3.75L5.25 19l-.25-.25 4.25-4.25L5.5 10.75 7 9.25l3.65.5 2.65-2.65 1.2-2.6Z" />
  `,
  unpin: `
    <path d="m4 4 16 16" />
    <path d="m14.5 4.5 5 5-2.6 1.2-1.9 1.9M9.5 9.5 7 9.25 5.5 10.75l3.75 3.75L5 18.75M13.25 16.5l1.5-1.5-.3-2.2" />
  `,
  search: `
    <circle cx="10.75" cy="10.75" r="6.25" />
    <path d="m15.5 15.5 4.25 4.25" />
  `,
  command: `
    <path d="M13 3.75 6.5 13h5l-.75 7.25L17.5 11h-5l.5-7.25Z" />
  `,
  attachment: `
    <path d="m14.5 7.5-6.6 6.6a2.12 2.12 0 0 0 3 3l7.3-7.3a4.24 4.24 0 0 0-6-6l-7.3 7.3a6.36 6.36 0 0 0 9 9l5.6-5.6" />
  `,
  image: `
    <rect x="3.75" y="4.75" width="16.5" height="14.5" rx="2" />
    <circle cx="8.75" cy="9.5" r="1.5" />
    <path d="m4.5 16.5 4.75-4.5 3.5 3.25 2.75-2.5 4 3.75" />
  `,
  file: `
    <path d="M7 3.75h6.75L18.5 8.5v11A1.75 1.75 0 0 1 16.75 21.25h-9.5A1.75 1.75 0 0 1 5.5 19.5V5.5A1.75 1.75 0 0 1 7.25 3.75Z" />
    <path d="M13.5 3.75V8.5h4.75" />
    <path d="M8.75 13h6.5M8.75 16.25h4.5" />
  `,
  screenshot: `
    <path d="M4 8V5.75A1.75 1.75 0 0 1 5.75 4H8M16 4h2.25A1.75 1.75 0 0 1 20 5.75V8M20 16v2.25A1.75 1.75 0 0 1 18.25 20H16M8 20H5.75A1.75 1.75 0 0 1 4 18.25V16" />
    <circle cx="12" cy="12" r="3.25" />
  `,
  sticker: `
    <path d="M12 3.75a8.25 8.25 0 0 0-8.25 8.25 8.25 8.25 0 0 0 8.25 8.25c.75 0 1.38-.28 1.9-.8l5.55-5.55c.52-.52.8-1.15.8-1.9A8.25 8.25 0 0 0 12 3.75Z" />
    <path d="M13.25 20.1V15.5a2 2 0 0 1 2-2h4.6" />
    <path d="M9 10.25h.01M15 10.25h.01" stroke-width="2.4" />
    <path d="M9.25 14a3.5 3.5 0 0 0 4 0" />
  `,
  poll: `
    <path d="M5 19.5V11M10 19.5V5M15 19.5v-6M20 19.5V8.5" />
  `,
  send: `
    <path d="M20.25 4.5 3.75 11.25l6.5 2.5 2.5 6.5L20.25 4.5Z" />
    <path d="m10.25 13.75 9.75-9" />
  `,
  reply: `
    <path d="M9.5 7.5 4.75 12l4.75 4.5" />
    <path d="M5 12h8.5a5.5 5.5 0 0 1 5.5 5.5v1" />
  `,
  forward: `
    <path d="m14.5 7.5 4.75 4.5-4.75 4.5" />
    <path d="M19 12h-8.5A5.5 5.5 0 0 0 5 17.5v1" />
  `,
  more: `
    <circle cx="6" cy="12" r="1.25" fill="currentColor" stroke="none" />
    <circle cx="12" cy="12" r="1.25" fill="currentColor" stroke="none" />
    <circle cx="18" cy="12" r="1.25" fill="currentColor" stroke="none" />
  `,
  settings: `
    <circle cx="12" cy="12" r="3" />
    <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09a1.65 1.65 0 0 0-1-1.51 1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09a1.65 1.65 0 0 0 1.51-1 1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33h.08a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82v.08a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1Z" />
  `,
  speaker: `
    <path d="M11 5.5 6.75 9H4.25v6h2.5L11 18.5v-13Z" />
    <path d="M14.75 8.75a4.5 4.5 0 0 1 0 6.5M17.5 6.25a8 8 0 0 1 0 11.5" />
  `,
  bellOff: `
    <path d="m4 4 16 16" />
    <path d="M9.3 4.6A5.5 5.5 0 0 1 17.5 9.5c0 2.6.6 4.4 1.5 5.5M6.2 6.2A5.5 5.5 0 0 0 6.5 9.5c0 3.5-1.5 5.5-2 6.25h11.25" />
    <path d="M10.25 19a2 2 0 0 0 3.5 0" />
  `,
  play: `
    <path d="M7.5 5.25 18.5 12 7.5 18.75V5.25Z" />
  `,
  pause: `
    <rect x="6.5" y="5.25" width="3.75" height="13.5" rx="1" />
    <rect x="13.75" y="5.25" width="3.75" height="13.5" rx="1" />
  `,
  close: `
    <path d="M6 6 18 18M18 6 6 18" />
  `,
  check: `
    <path d="m5 12.5 4.5 4.5 9.5-9.5" />
  `,
  checkDouble: `
    <path d="m3.5 12.5 4 4 8.5-8.5" />
    <path d="m9.5 12.5 4 4 7-7" />
  `,
  plus: `
    <path d="M12 5v14M5 12h14" />
  `,
  star: `
    <path d="m12 3.75 2.55 5.18 5.7.83-4.12 4.02.97 5.68L12 16.78l-5.1 2.68.97-5.68-4.12-4.02 5.7-.83L12 3.75Z" />
  `,
  trash: `
    <path d="M4.5 6.5h15M9.25 6.5V4.75a1 1 0 0 1 1-1h3.5a1 1 0 0 1 1 1V6.5M6.75 6.5l.75 12.25a1.75 1.75 0 0 0 1.75 1.65h5.5a1.75 1.75 0 0 0 1.75-1.65L17.25 6.5" />
    <path d="M10.25 10.5v5.5M13.75 10.5v5.5" />
  `,
  refresh: `
    <path d="M20 11a8 8 0 1 0-2.1 5.4" />
    <path d="M20 5.5V11h-5.5" />
  `,
  sync: `
    <rect x="4" y="5" width="7.5" height="14" rx="1.5" />
    <path d="M14.5 8h5.5v8.5h-5.5M13 16.5h8" />
    <path d="m11.5 11.5 3-2v4l-3-2Z" />
  `,
  inspector: `
    <rect x="3.75" y="4.75" width="16.5" height="14.5" rx="2" />
    <path d="M14.5 4.75v14.5" />
  `,
  sidebar: `
    <rect x="3.75" y="4.75" width="16.5" height="14.5" rx="2" />
    <path d="M9.25 4.75v14.5" />
  `,
  sidebarCollapse: `
    <rect x="3.75" y="4.75" width="16.5" height="14.5" rx="2" />
    <path d="M9 4.75v14.5" />
    <path d="m15.75 9.75-2.25 2.25 2.25 2.25" />
  `,
  sidebarExpand: `
    <rect x="3.75" y="4.75" width="16.5" height="14.5" rx="2" />
    <path d="M9 4.75v14.5" />
    <path d="m13.5 9.75 2.25 2.25-2.25 2.25" />
  `,
  tag: `
    <path d="M4.5 11.5V5.75A1.25 1.25 0 0 1 5.75 4.5h5.75l8 8-6 6-8-8Z" />
    <circle cx="8.25" cy="8.25" r="1" fill="currentColor" stroke="none" />
  `,
  smile: `
    <circle cx="12" cy="12" r="8.25" />
    <path d="M9 10h.01M15 10h.01" stroke-width="2.4" />
    <path d="M8.75 14.25a4.5 4.5 0 0 0 6.5 0" />
  `,
  download: `
    <path d="M12 4.25v10.5M8.25 11.25 12 15l3.75-3.75" />
    <path d="M4.75 17.25v1A1.75 1.75 0 0 0 6.5 20h11a1.75 1.75 0 0 0 1.75-1.75v-1" />
  `,
  folder: `
    <path d="M3.75 7A1.75 1.75 0 0 1 5.5 5.25h4l2 2.25h7A1.75 1.75 0 0 1 20.25 9.25v8.5a1.75 1.75 0 0 1-1.75 1.75H5.5a1.75 1.75 0 0 1-1.75-1.75V7Z" />
  `,
  externalLink: `
    <path d="M14 4.5h5.5V10M19.25 4.75 11 13" />
    <path d="M10.5 6H6.25A1.75 1.75 0 0 0 4.5 7.75v10A1.75 1.75 0 0 0 6.25 19.5h10A1.75 1.75 0 0 0 18 17.75V13.5" />
  `,
  phone: `
    <path d="M6.6 4.5h2.6l1.3 3.4-1.9 1.6a12.5 12.5 0 0 0 5.9 5.9l1.6-1.9 3.4 1.3v2.6a1.8 1.8 0 0 1-1.95 1.8A15.2 15.2 0 0 1 4.8 6.45 1.8 1.8 0 0 1 6.6 4.5Z" />
  `,
  video: `
    <rect x="3.75" y="6.25" width="11.5" height="11.5" rx="2" />
    <path d="m15.25 10.25 5-2.75v9l-5-2.75v-3.5Z" />
  `,
  mapPin: `
    <path d="M12 20.25s6.25-5.15 6.25-10A6.25 6.25 0 1 0 5.75 10.25c0 4.85 6.25 10 6.25 10Z" />
    <circle cx="12" cy="10.25" r="2.25" />
  `,
  alert: `
    <path d="M12 4.25 3.5 19h17L12 4.25Z" />
    <path d="M12 10v3.75" />
    <circle cx="12" cy="16.5" r="0.65" fill="currentColor" stroke="none" />
  `,
  logout: `
    <path d="M9.5 4.5H6.25A1.75 1.75 0 0 0 4.5 6.25v11.5c0 .97.78 1.75 1.75 1.75H9.5" />
    <path d="M15.25 8.25 19.5 12l-4.25 3.75M9.5 12h9.75" />
  `,
  terminal: `
    <rect x="3.75" y="4.75" width="16.5" height="14.5" rx="2" />
    <path d="m7.75 9.5 3 2.5-3 2.5M12.75 14.5h3.5" />
  `,
  copy: `
    <rect x="8.5" y="8.5" width="10.75" height="10.75" rx="1.75" />
    <path d="M15.5 8.5V6.25A1.5 1.5 0 0 0 14 4.75H6.25a1.5 1.5 0 0 0-1.5 1.5V14a1.5 1.5 0 0 0 1.5 1.5H8.5" />
  `,
  atSign: `
    <circle cx="12" cy="12" r="3.5" />
    <path d="M15.5 12v1.5a2.25 2.25 0 0 0 4.5 0V12a8 8 0 1 0-4.4 7.15" />
  `,
  clock: `
    <circle cx="12" cy="12" r="8.25" />
    <path d="M12 7.5V12l3 2" />
  `,
  chevronDown: `
    <path d="m7 9.5 5 5 5-5" />
  `,
  chevronRight: `
    <path d="m9.5 7 5 5-5 5" />
  `,
};

/**
 * Renders a crisp 24x24 inline SVG icon with 1.75px stroke.
 * @param {keyof typeof PATHS} name
 * @param {string} [className="ui-icon"]
 * @returns {string}
 */
export function icon(name, className = 'ui-icon') {
  const body = PATHS[name] || PATHS.inbox;
  return `<svg class="${className}" viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">${body}</svg>`;
}

export const ICON_NAMES = Object.keys(PATHS);
