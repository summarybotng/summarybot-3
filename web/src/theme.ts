// Per-tenant theming (TEN-003, MVP). The Tenant model has no brand-colour field
// yet, so we derive a stable accent from the tenant id — different tenants look
// visibly different. When a real brand_colour lands, swap the derivation for it.

function hashHue(seed: string): number {
  let h = 0
  for (let i = 0; i < seed.length; i++) h = (h * 31 + seed.charCodeAt(i)) >>> 0
  return h % 360
}

/** Set the brand accent from a tenant id (or reset to default when absent). */
export function applyTenantAccent(tenantId: string | null) {
  const root = document.documentElement
  if (!tenantId) {
    root.style.removeProperty('--accent')
    return
  }
  const hue = hashHue(tenantId)
  root.style.setProperty('--accent', `hsl(${hue} 70% 45%)`)
  root.style.setProperty('--accent-fg', '#ffffff')
}
