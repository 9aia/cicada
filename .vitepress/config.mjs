import { defineConfig } from 'vitepress'

export default defineConfig({
  title: 'cicada',
  description: 'Calendars as code — declare calendars in YAML, plan/apply/drift like infrastructure.',
  base: '/cicada/',
  themeConfig: {
    nav: [
      { text: 'Tutorials', link: '/tutorials/getting-started' },
      { text: 'How-to', link: '/how-to/' },
      { text: 'Reference', link: '/reference/cli' },
      { text: 'Explanation', link: '/explanation/' },
      { text: '9aia', link: 'https://9aia.com' },
    ],
    sidebar: [
      {
        text: 'Tutorials',
        items: [{ text: 'Getting started', link: '/tutorials/getting-started' }],
      },
      {
        text: 'How-to',
        items: [
          { text: 'Connect Google Calendar', link: '/how-to/connect-google' },
          { text: 'Reconcile a calendar', link: '/how-to/reconcile-a-calendar' },
          { text: 'Manage recurring events', link: '/how-to/manage-recurrence' },
          { text: 'Use cicada from an agent', link: '/how-to/use-from-an-agent' },
          { text: 'Keep state safe', link: '/how-to/keep-state-safe' },
          { text: 'Run cicada in CI', link: '/how-to/run-in-ci' },
        ],
      },
      {
        text: 'Reference',
        items: [
          { text: 'CLI', link: '/reference/cli' },
          { text: 'YAML DSL', link: '/reference/yaml-dsl' },
          { text: 'MCP tools', link: '/reference/mcp-tools' },
          { text: 'State file', link: '/reference/state-file' },
          { text: 'Providers', link: '/reference/providers' },
          { text: 'Error taxonomy', link: '/reference/errors' },
        ],
      },
      {
        text: 'Explanation',
        items: [
          { text: 'Architecture', link: '/explanation/architecture' },
          { text: 'Plan, apply and drift', link: '/explanation/plan-apply-drift' },
          { text: 'The landscape', link: '/explanation/landscape' },
        ],
      },
    ],
    socialLinks: [{ icon: 'github', link: 'https://github.com/9aia/cicada' }],
  },
})
