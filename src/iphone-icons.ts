const paths = {
  account: '<circle cx="9" cy="8" r="3"/><path d="M3 20v-2a4 4 0 0 1 4-4h4a4 4 0 0 1 4 4v2m1-15a3 3 0 0 1 0 6m5 9v-2a4 4 0 0 0-3-4"/>',
  manage: '<path d="M4 7h7m4 0h5M4 17h3m4 0h9"/><circle cx="13" cy="7" r="2"/><circle cx="9" cy="17" r="2"/>',
  add: '<path d="M12 5v14M5 12h14"/>',
  update: '<path d="M12 3v12m-4-4 4 4 4-4M5 17v3h14v-3"/>',
  search: '<circle cx="10.5" cy="10.5" r="6.5"/><path d="m16 16 4 4"/>',
  info: '<circle cx="12" cy="12" r="9"/><path d="M12 11v5m0-8h.01"/>',
  approval: '<path d="M12 22s7-3.5 7-9V5l-7-3-7 3v8c0 5.5 7 9 7 9Z"/><path d="M12 8v4m0 3h.01"/>',
  proxy: '<rect x="9" y="3" width="6" height="5" rx="1"/><rect x="3" y="16" width="6" height="5" rx="1"/><rect x="15" y="16" width="6" height="5" rx="1"/><path d="M12 8v4m-6 4v-4h12v4"/>',
  login: '<circle cx="8" cy="15" r="4"/><path d="m11 12 8-8m-2 2 3 3m-6 0 2 2"/>',
  chat: '<path d="M20 11.5a8 8 0 0 1-8 8H5l1.4-3.5a8 8 0 1 1 13.6-4.5Z"/><path d="M9 11.5h6"/>',
  task: '<path d="M6 17h12l-1.5-3v-3a4.5 4.5 0 0 0-9 0v3ZM10 20a2 2 0 0 0 4 0"/>',
  backup: '<rect x="4" y="4" width="16" height="5" rx="1"/><path d="M5 9v11h14V9m-9 4h4"/>',
  pulse: '<path d="M3 12h4l3-7 4 14 3-7h4"/>',
  globe: '<circle cx="12" cy="12" r="9"/><ellipse cx="12" cy="12" rx="4" ry="9"/><path d="M3 12h18"/>',
  cookie: '<path d="M20 11a8 8 0 1 1-8-8 4 4 0 0 0 4 4 4 4 0 0 0 4 4Z"/><path d="M8 12h.01m5 3h.01m-3-7h.01"/>',
  eye: '<path d="M2 12s3.5-6 10-6 10 6 10 6-3.5 6-10 6-10-6-10-6Z"/><circle cx="12" cy="12" r="2.5"/>',
  eyeOff: '<path d="m3 3 18 18M10 6a13 13 0 0 1 2 0c6.5 0 10 6 10 6a16 16 0 0 1-3 3.5M16 17a12 12 0 0 1-4 1c-6.5 0-10-6-10-6a16 16 0 0 1 4.2-4.4"/>',
  quota: '<path d="M5 18a9 9 0 1 1 14 0M12 14l4-5"/>',
} as const;

export function iphoneIcon(name: keyof typeof paths): string {
  return `<svg aria-hidden="true" width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.65" stroke-linecap="round" stroke-linejoin="round">${paths[name]}</svg>`;
}
