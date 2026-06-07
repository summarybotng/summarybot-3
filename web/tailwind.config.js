/** @type {import('tailwindcss').Config} */
export default {
  content: ['./index.html', './src/**/*.{ts,tsx}'],
  theme: {
    extend: {
      // Per-tenant brand colour, driven by a CSS variable set at runtime from
      // the resolved tenant (see theme.ts). Defaults in index.css.
      colors: {
        accent: 'var(--accent)',
        'accent-fg': 'var(--accent-fg)',
      },
    },
  },
  plugins: [],
}
