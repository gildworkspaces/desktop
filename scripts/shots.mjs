// Screenshots of the real app for gild.addis.gg: the built binary, driven
// through tauri-driver like tests/e2e/smoke.mjs, on a workspace this script
// fills through the app's own commands (and, for the agents, through the MCP
// endpoint they use). Each screen is taken in light and in dark.
//   ci/linux.sh shots          (writes shots/<name>-light.png, -dark.png)
//   tauri-driver &   node scripts/shots.mjs <built binary> <out dir>
import { Builder, By, until } from 'selenium-webdriver'
import { mkdirSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'

const [application, out = 'shots'] = process.argv.slice(2)
mkdirSync(out, { recursive: true })
const driver = await new Builder()
  .usingServer('http://127.0.0.1:4444/')
  .withCapabilities({ browserName: 'wry', 'tauri:options': { application } })
  .build()

const call = (cmd, args = {}) => driver.executeAsyncScript(`
  const done = arguments[arguments.length - 1]
  window.__TAURI_INTERNALS__.invoke(arguments[0], arguments[1]).then(r => done({ ok: r }), e => done({ error: String(e) }))
`, cmd, args).then(r => { if (r.error) throw new Error(`${cmd}: ${r.error}`); return r.ok })

// An agent's calls, with its own token, as Claude Code or Codex would make them.
async function agent(token) {
  const post = async (body, session) => {
    const res = await fetch('http://127.0.0.1:4747/mcp', {
      method: 'POST', body: JSON.stringify(body),
      headers: { 'Content-Type': 'application/json', Accept: 'application/json, text/event-stream', Authorization: `Bearer ${token}`, ...(session ? { 'Mcp-Session-Id': session } : {}) },
    })
    const text = await res.text()
    const line = text.split('\n').map(l => l.replace(/^data:\s*/, '')).find(l => l.startsWith('{'))
    return { session: res.headers.get('mcp-session-id'), json: line ? JSON.parse(line) : null }
  }
  const { session } = await post({ jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2025-03-26', capabilities: {}, clientInfo: { name: 'shots', version: '1' } } })
  await post({ jsonrpc: '2.0', method: 'notifications/initialized' }, session)
  let id = 2
  return async (name, args) => {
    const r = await post({ jsonrpc: '2.0', id: id++, method: 'tools/call', params: { name, arguments: args } }, session)
    if (r.json.result.isError) throw new Error(`${name}: ${r.json.result.content[0].text}`)
    return JSON.parse(r.json.result.content[0].text)
  }
}

const pause = ms => new Promise(r => setTimeout(r, ms))
async function shot(name) {
  for (const theme of ['light', 'dark']) {
    // The window sets the theme from your profile whenever it re-renders; hold this one while the shot is taken.
    await driver.executeScript(`
      const root = document.documentElement
      window.__shotTheme = arguments[0]
      if (!window.__shotHold) {
        window.__shotHold = new MutationObserver(() => { if (root.dataset.theme !== window.__shotTheme) root.dataset.theme = window.__shotTheme })
        window.__shotHold.observe(root, { attributes: true, attributeFilter: ['data-theme'] })
      }
      root.dataset.theme = window.__shotTheme
    `, theme)
    await pause(400)
    writeFileSync(join(out, `${name}-${theme}.png`), await driver.takeScreenshot(), 'base64')
    console.log('shot:', `${name}-${theme}.png`)
  }
}
const click = async (by, what) => (await driver.wait(until.elementLocated(by), 15000, what)).click()

try {
  await driver.manage().window().setRect({ width: 1440, height: 900 })
  await driver.wait(until.elementLocated(By.css('button.card')), 30000)

  // You, a workspace and its channels.
  await call('save_profile', { name: 'Alex Rivera', bio: 'Dean of admissions', picture: null, banner: '#3b6fd8' })
  const ws = await call('create_workspace', { name: 'Riverside College' })
  const general = (await call('channels', { workspaceId: ws.id }))[0]
  const intake = await call('create_channel', { workspaceId: ws.id, name: 'student-intake', topic: 'New students, from referral to enrolled' })
  const aid = await call('create_channel', { workspaceId: ws.id, name: 'financial-aid', topic: 'Grants, loans and work-study' })
  await call('create_channel', { workspaceId: ws.id, name: 'registrar', topic: 'Transcripts and course records' })

  // Agents, each with a token, as gild's setup makes them.
  const made = {}
  for (const name of ['intake', 'registrar', 'aid']) made[name] = await call('create_agent', { name })
  const as = {}
  for (const [name, a] of Object.entries(made)) as[name] = await agent(a.token)
  await call('save_agent_profile', { agentId: made.intake.agent.id, picture: null, prompt: 'You run student intake at Riverside College. Keep the board current: every referral is a card, and a card moves when its paperwork does. Answer questions from the Intake SOP, and say which step you used.' })

  // The org: you lead it; agents hold seats under open ones.
  let orgs = await call('orgs')
  let org = orgs.find(o => o.workspaces.includes(ws.id)) ?? orgs[0]
  orgs = await call('rename_org', { orgId: org.id, name: 'Riverside College' })
  const seat = title => orgs.flatMap(o => o.seats).find(s => s.title === title)
  const ceo = orgs.find(o => o.id === org.id).seats.find(s => s.parent == null)
  orgs = await call('set_seat', { seatId: ceo.id, title: 'Dean', holder: ceo.holder })
  orgs = await call('add_seat', { parentId: ceo.id, title: 'Director of Admissions' })
  orgs = await call('add_seat', { parentId: ceo.id, title: 'Director of Student Services' })
  orgs = await call('add_seat', { parentId: seat('Director of Admissions').id, title: 'Intake Lead' })
  orgs = await call('add_seat', { parentId: seat('Director of Admissions').id, title: 'Records Lead' })
  orgs = await call('add_seat', { parentId: seat('Director of Student Services').id, title: 'Financial Aid Lead' })
  orgs = await call('set_seat', { seatId: seat('Intake Lead').id, title: 'Intake Lead', holder: { kind: 'agent', id: made.intake.agent.id } })
  orgs = await call('set_seat', { seatId: seat('Records Lead').id, title: 'Records Lead', holder: { kind: 'agent', id: made.registrar.agent.id } })
  orgs = await call('set_seat', { seatId: seat('Financial Aid Lead').id, title: 'Financial Aid Lead', holder: { kind: 'agent', id: made.aid.agent.id } })
  // Financial aid is for Student Services' team, not the whole college.
  await call('set_channel_rules', { channelId: aid.id, org: false, teams: [seat('Director of Student Services').id] })

  // #student-intake: a board of referrals, kept by intake.
  await call('set_view', { channelId: intake.id, fields: ['Program', 'Sent by'], statuses: ['New', 'Docs needed', 'Enrolled'] })
  const students = [
    ['Jordan Lee', 'Docs needed', 'Nursing', 'Northside High'],
    ['Priya Shah', 'Enrolled', 'Computer Science', 'Eastgate Academy'],
    ['Marcus Bell', 'New', 'Welding', 'Northside High'],
    ['Sofia Alvarez', 'Docs needed', 'Early Childhood', 'Lakeview High'],
    ['Wei Chen', 'Enrolled', 'Accounting', 'Transfer'],
    ['Amara Okafor', 'New', 'Nursing', 'Eastgate Academy'],
    ['Liam Murphy', 'New', 'Automotive', 'Lakeview High'],
    ['Hana Sato', 'Enrolled', 'Graphic Design', 'Northside High'],
  ]
  for (const [title, status, program, from] of students) await as.intake('add_record', { channel_id: intake.id, title, status, fields: { Program: program, 'Sent by': from } })

  // The conversation under it.
  await call('send_message', { channelId: intake.id, body: 'Northside sent three more students this morning.' })
  await as.intake('post_message', { channel_id: intake.id, text: 'Added Marcus Bell, Amara Okafor and Liam Murphy as New. Jordan Lee is still waiting on a transcript.' })
  await call('send_message', { channelId: intake.id, body: '@registrar can you request Jordan’s transcript from Northside?' })
  await as.registrar('post_message', { channel_id: intake.id, text: 'Requested it from Northside’s office. I’ll post here when it arrives, usually two business days.' })
  await call('send_message', { channelId: intake.id, body: '@intake where do signed forms go now?' })
  await as.intake('post_message', { channel_id: intake.id, text: 'In the red folder, top drawer (Intake SOP, Step 2).' })

  // A doc, and an agent's proposed edit waiting for a person.
  const doc = await call('create_doc', { channelId: intake.id, title: 'Intake SOP' })
  await call('save_doc', { channelId: intake.id, path: doc.path, body: '# Intake SOP\n\n## Step 1: Referral\n\nEvery referral becomes a card on the board, as New.\n\n## Step 2: Forms\n\nSigned intake forms go in the blue folder.\n\n## Step 3: Enrolled\n\nMove the card when the registrar confirms the schedule.\n' })
  await as.intake('propose_doc_edit', {
    channel_id: intake.id, path: doc.path, summary: 'Forms moved to the red folder',
    text: '# Intake SOP\n\n## Step 1: Referral\n\nEvery referral becomes a card on the board, as New.\n\n## Step 2: Forms\n\nSigned intake forms go in the red folder, top drawer.\n\n## Step 3: Enrolled\n\nMove the card when the registrar confirms the schedule.\n',
  })
  await call('send_message', { channelId: general.id, body: 'Welcome to Riverside College’s workspace. Admissions works in #student-intake.' })

  // #financial-aid: awards, kept by aid, for Student Services' team.
  await call('set_view', { channelId: aid.id, fields: ['Award', 'Amount'], statuses: ['Applied', 'In review', 'Awarded'] })
  for (const [title, status, award, amount] of [
    ['Priya Shah', 'Awarded', 'State grant', '$4,200'],
    ['Jordan Lee', 'In review', 'Work-study', '$2,800'],
    ['Wei Chen', 'Awarded', 'Transfer scholarship', '$1,500'],
    ['Sofia Alvarez', 'Applied', 'State grant', '$3,900'],
    ['Hana Sato', 'In review', 'Need-based grant', '$2,100'],
  ]) await as.aid('add_record', { channel_id: aid.id, title, status, fields: { Award: award, Amount: amount } })
  await call('send_message', { channelId: aid.id, body: '@aid which awards are still in review?' })
  await as.aid('post_message', { channel_id: aid.id, text: 'Two: Jordan Lee (work-study) and Hana Sato (need-based grant). Both are waiting on a signed form.' })

  // Open it, as a person would.
  await driver.navigate().refresh()
  await click(By.xpath("//button[contains(@class,'card') and contains(., 'Riverside College')]"), 'the workspace')
  await click(By.xpath("//button[contains(@class,'chan') and contains(., 'student-intake')]"), '#student-intake')
  await click(By.xpath("//div[@aria-label='Show']//button[.='Board']"), 'Board')
  await driver.wait(until.elementLocated(By.xpath("//span[@class='text' and contains(., 'red folder')]")), 15000)
  await shot('channel')

  await click(By.xpath("//div[@aria-label='Show']//button[starts-with(normalize-space(.),'Docs')]"), 'Docs')
  // Docs opens on the change waiting for you; Expand view gives the diff the room.
  await driver.wait(until.elementLocated(By.xpath("//div[contains(@class,'dl') and contains(@class,'add')]")), 15000)
  await click(By.xpath("//button[.='Expand view']"), 'Expand view')
  await shot('review')
  await click(By.xpath("//button[.='Collapse view']"), 'Collapse view')

  await click(By.xpath("//button[contains(@class,'chan') and contains(., 'financial-aid')]"), '#financial-aid')
  await click(By.xpath("//button[contains(@class,'ghost') and .='Access']"), 'Access')
  await driver.wait(until.elementLocated(By.css('#rule-org')), 15000)
  await shot('access')
  await click(By.xpath("//aside[contains(@class,'panel')]//button[.='Close']"), 'Close')

  await click(By.css("button[aria-label='App settings']"), 'Settings')
  await click(By.xpath("//button[contains(@class,'sitem') and .='Org']"), 'Org')
  await click(By.css("button.seat[aria-label='Intake Lead, intake']"), 'the intake seat')
  await driver.wait(until.elementLocated(By.css("aside[aria-label='Seat: Intake Lead']")), 15000)
  await shot('org')

  await click(By.xpath("//button[contains(@class,'sitem') and .='Agents']"), 'Agents')
  await click(By.css("button[aria-label='intake: profile']"), 'intake profile')
  await driver.wait(until.elementLocated(By.css('.chainrow.me')), 15000)
  await shot('profile')
} catch (e) {
  console.log('failed; the window says:', await driver.executeScript('return document.body.innerText').catch(() => '?'))
  writeFileSync(join(out, 'failed.png'), await driver.takeScreenshot(), 'base64')
  throw e
} finally {
  await driver.quit()
}
