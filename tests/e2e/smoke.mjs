// The built app, driven through tauri-driver (Linux CI, under Xvfb): the
// window opens on the seeded demo workspace, and what you type reaches the
// Rust core and comes back from its database.
//   tauri-driver &   node tests/e2e/smoke.mjs <path to the built binary>
import { Builder, By, Key, until } from 'selenium-webdriver'
import assert from 'node:assert/strict'
import { readFileSync, existsSync, mkdtempSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

const application = process.argv[2]
const driver = await new Builder()
  .usingServer('http://127.0.0.1:4444/')
  .withCapabilities({ browserName: 'wry', 'tauri:options': { application } })
  .build()
try {
  // First run: the demo workspace is there, signed in as the demo account.
  const card = await driver.wait(until.elementLocated(By.css('button.card')), 30000)
  assert.match(await card.getText(), /Demo workspace/)
  assert.match(await driver.findElement(By.css('.who')).getText(), /demo account/)

  // Open it: #general holds the welcome message from the seed.
  await card.click()
  await driver.wait(until.elementLocated(By.css('.msg')), 10000)
  assert.match(await driver.findElement(By.css('.list')).getText(), /Welcome to gild/)

  // A new channel and a message, through the window like a person would.
  await driver.findElement(By.css('#new-channel')).sendKeys('Student Intake', Key.ENTER)
  await driver.wait(until.elementLocated(By.xpath("//h2[contains(., 'student-intake')]")), 10000)
  await driver.findElement(By.css('#composer')).sendKeys('Northside sent three more students.', Key.ENTER)
  await driver.wait(until.elementLocated(By.xpath("//span[@class='text' and contains(., 'Northside sent three more students.')]")), 10000)

  // The core has it: read back through the command, not the screen.
  const saved = await driver.executeAsyncScript(`
    const done = arguments[arguments.length - 1]
    const call = (cmd, args) => window.__TAURI_INTERNALS__.invoke(cmd, args)
    call('workspaces').then(async ws => {
      const chans = await call('channels', { workspaceId: ws[0].id })
      const intake = chans.find(c => c.name === 'student-intake')
      done({ channels: chans.map(c => c.name), messages: (await call('messages', { channelId: intake.id })).map(m => m.author + ': ' + m.body) })
    }, e => done({ error: String(e) }))
  `)
  assert.equal(saved.error, undefined, saved.error)
  assert.deepEqual(saved.channels, ['general', 'student-intake'])
  assert.deepEqual(saved.messages, ['Demo: Northside sent three more students.'])
  console.log('saved:', JSON.stringify(saved))

  // The channel gets a table through the window, and a record's history comes
  // back from git through the core.
  await driver.findElement(By.css('#cols')).sendKeys('Sent by, Program')
  const stats = await driver.findElement(By.css('#stats'))
  await stats.clear()
  await stats.sendKeys('New, Docs needed, Enrolled')
  await driver.findElement(By.css('form.columns button.primary')).click()
  await driver.wait(until.elementLocated(By.css('#rec-title')), 10000)
  await driver.findElement(By.css('#rec-title')).sendKeys('Jordan Lee', Key.ENTER)
  await driver.wait(until.elementLocated(By.xpath("//td//button[contains(., 'Jordan Lee')]")), 10000)
  const history = await driver.executeAsyncScript(`
    const done = arguments[arguments.length - 1]
    const call = (cmd, args) => window.__TAURI_INTERNALS__.invoke(cmd, args)
    ;(async () => {
      const ws = await call('workspaces')
      const intake = (await call('channels', { workspaceId: ws[0].id })).find(c => c.name === 'student-intake')
      const data = await call('channel_data', { channelId: intake.id })
      const jordan = data.records[0]
      await call('update_record', { channelId: intake.id, recordId: jordan.id, title: jordan.title, status: 'Docs needed', fields: { 'Sent by': 'Northside' } })
      return (await call('record_history', { channelId: intake.id, recordId: jordan.id })).map(h => h.author + ': ' + h.message)
    })().then(done, e => done({ error: String(e) }))
  `)
  assert.equal(history.error, undefined, history.error)
  assert.deepEqual(history, ['Demo: Jordan Lee: New → Docs needed, Sent by: Northside', 'Demo: Add Jordan Lee'])
  console.log('history:', JSON.stringify(history))

  // An agent, added in the window: gild walks through setting it up. CI has
  // no AI tool gild can set up (clients.rs's tests cover that), so it takes
  // the token for another MCP client, then the wake command, then a channel.
  const work = mkdtempSync(join(tmpdir(), 'gild-e2e-'))
  writeFileSync(join(work, 'agent.mjs'), `
    import { readFileSync } from 'node:fs'
    const prompt = readFileSync(0, 'utf8')
    const channel = /channel_id (\\S+)\\)/.exec(prompt)[1]
    const token = readFileSync(process.argv[2], 'utf8').trim()
    const post = async (body, session) => {
      const res = await fetch('http://127.0.0.1:4747/mcp', { method: 'POST', body: JSON.stringify(body), headers: {
        'Content-Type': 'application/json', Accept: 'application/json, text/event-stream', Authorization: 'Bearer ' + token, ...(session ? { 'Mcp-Session-Id': session } : {}) } })
      await res.text()
      return res.headers.get('mcp-session-id')
    }
    const session = await post({ jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2025-03-26', capabilities: {}, clientInfo: { name: 'woken', version: '1' } } })
    await post({ jsonrpc: '2.0', method: 'notifications/initialized' }, session)
    const asked = (prompt.includes('where do the forms go?') ? 'They go in the red folder (Intake SOP, Step 2).' : 'I could not find the question.')
      + (prompt.includes(': Northside sent three more students.') ? ' I saw Northside sent three more.' : '')
    await post({ jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'post_message', arguments: { channel_id: channel, text: asked } } }, session)
  `)
  await driver.findElement(By.css('#new-agent')).sendKeys('intake', Key.ENTER)
  await driver.wait(until.elementLocated(By.xpath("//div[contains(@class,'setup')]//h2[.='Set up intake']")), 10000)
  await driver.wait(until.elementLocated(By.xpath("//label[contains(@class,'clientrow') and contains(., 'Claude Code')]")), 30000)
  await driver.findElement(By.xpath("//details[contains(@class,'manual')]/summary")).click()
  await driver.findElement(By.xpath("//button[normalize-space(.)='Show a token']")).click()
  const token = await (await driver.wait(until.elementLocated(By.css('.secret code')), 30000)).getText()
  assert.match(token, /^gild_[0-9a-f]{64}$/)
  writeFileSync(join(work, 'token'), token)
  await driver.findElement(By.xpath("//div[contains(@class,'setup')]//button[normalize-space(.)='Next']")).click()
  const intakeBox = await driver.wait(until.elementLocated(By.xpath("//label[contains(@class,'clientrow') and contains(., '#student-intake')]/input")), 10000)
  assert.ok(await intakeBox.isSelected(), 'the channel you are in is ticked')
  assert.ok(!(await driver.findElement(By.xpath("//label[contains(@class,'clientrow') and contains(., '#general')]/input")).isSelected()))
  await driver.findElement(By.xpath("//div[contains(@class,'setup')]//button[normalize-space(.)='Save']")).click()
  // No AI tool here, so nothing can wake it yet, and the last step says so.
  const doneText = await driver.wait(until.elementLocated(By.xpath("//div[contains(@class,'setup')]//p[contains(., 'can’t wake it yet')]")), 10000)
  assert.match(await doneText.getText(), /under Mentions/)
  await driver.findElement(By.xpath("//div[contains(@class,'setup')]//button[normalize-space(.)='Done']")).click()

  // Its settings, under Mentions: what runs (this test's stand-in for an AI
  // tool) and how much of the conversation comes with a mention.
  await driver.findElement(By.xpath("//button[contains(@class,'agentrow') and contains(., 'intake')]")).click()
  await driver.wait(until.elementLocated(By.xpath("//ol[contains(@class,'steps')]//button[contains(., 'Mentions')]")), 10000).click()
  const ctx = await driver.wait(until.elementLocated(By.css('#context')), 10000)
  assert.equal(await ctx.getAttribute('value'), '20')
  await ctx.clear()
  await ctx.sendKeys('50')
  await driver.findElement(By.css('#wake-command')).sendKeys(`${process.execPath} ${join(work, 'agent.mjs')} ${join(work, 'token')}`)
  await driver.findElement(By.xpath("//form[contains(@class,'stepform')]//button[normalize-space(.)='Save']")).click()
  await driver.wait(until.elementLocated(By.xpath("//div[contains(@class,'setup')]//span[@class='ok' and .='Saved.']")), 10000)
  await driver.findElement(By.xpath("//div[contains(@class,'setup')]//button[normalize-space(.)='Close']")).click()
  await driver.wait(until.elementLocated(By.xpath("//button[contains(@class,'agentrow')]//span[contains(., 'wakes on @')]")), 10000)
  const agentsNow = await driver.executeAsyncScript(`
    const done = arguments[arguments.length - 1]
    window.__TAURI_INTERNALS__.invoke('agents').then(a => done(a.map(x => x.name + ':' + x.context + ':' + x.wake)), e => done({ error: String(e) }))
  `)
  assert.deepEqual(agentsNow, [`intake:50:${process.execPath} ${join(work, 'agent.mjs')} ${join(work, 'token')}`], 'the whole command was saved')
  const mcp = async (body, session) => {
    const res = await fetch('http://127.0.0.1:4747/mcp', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Accept: 'application/json, text/event-stream', Authorization: `Bearer ${token}`, ...(session ? { 'Mcp-Session-Id': session } : {}) },
      body: JSON.stringify(body),
    })
    const text = await res.text()
    const line = text.split('\n').map(l => l.replace(/^data:\s*/, '')).find(l => l.startsWith('{'))
    return { session: res.headers.get('mcp-session-id'), json: line ? JSON.parse(line) : null }
  }
  const init = await mcp({ jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2025-03-26', capabilities: {}, clientInfo: { name: 'e2e', version: '1' } } })
  assert.equal(init.json.result.serverInfo.name, 'gild')
  await mcp({ jsonrpc: '2.0', method: 'notifications/initialized' }, init.session)
  const listed = await mcp({ jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'list_channels', arguments: {} } }, init.session)
  const channels = JSON.parse(listed.json.result.content[0].text).channels.map(c => `${c.name}:${c.access}`)
  assert.deepEqual(channels, ['student-intake:write'], 'general was never shared')
  console.log('agent sees:', JSON.stringify(channels))

  // Docs: a person writes one in the window; the agent finds the answer in
  // it, and its edit waits until the person approves it in the window.
  await driver.findElement(By.xpath("//button[contains(@class,'tab') and starts-with(normalize-space(.),'Docs')]")).click()
  await driver.wait(until.elementLocated(By.css('#new-doc')), 10000).sendKeys('Intake SOP', Key.ENTER)
  await driver.wait(until.elementLocated(By.xpath("//article[contains(@class,'doc')]//button[normalize-space(.)='Edit']")), 10000).click()
  const editor = await driver.wait(until.elementLocated(By.css('#doc-body')), 10000)
  await editor.clear()
  await editor.sendKeys('# Intake SOP', Key.ENTER, Key.ENTER, '## Step 2: Forms', Key.ENTER, Key.ENTER, 'Signed intake forms go in the blue folder.', Key.ENTER)
  await driver.findElement(By.xpath("//article[contains(@class,'doc')]//button[normalize-space(.)='Save']")).click()
  await driver.wait(until.elementLocated(By.xpath("//div[@class='md']//h3[contains(., 'Step 2: Forms')]")), 10000)

  const tool = async (id, name, args) => {
    const r = await mcp({ jsonrpc: '2.0', id, method: 'tools/call', params: { name, arguments: args } }, init.session)
    assert.ok(!r.json.result.isError, r.json.result.content[0].text)
    return JSON.parse(r.json.result.content[0].text)
  }
  const found = await tool(3, 'search_docs', { query: 'Where do the intake forms go?' })
  assert.deepEqual([found.hits[0].title, found.hits[0].section, found.hits[0].excerpt], ['Intake SOP', 'Step 2: Forms', 'Signed intake forms go in the blue folder.'])
  console.log('agent found:', JSON.stringify(found.hits[0]))
  const intakeId = (await tool(4, 'list_channels', {})).channels[0].channel_id
  const [doc] = (await tool(5, 'read_channel', { channel_id: intakeId })).docs
  const proposed = await tool(6, 'propose_doc_edit', {
    channel_id: intakeId, path: doc.path,
    text: '# Intake SOP\n\n## Step 2: Forms\n\nSigned intake forms go in the red folder.\n', summary: 'The folder is red now',
  })
  assert.match(proposed.status, /waiting/)

  // The window shows it waiting (refreshed by the agent's change), and the
  // doc still says blue until the person approves.
  await driver.wait(until.elementLocated(By.xpath("//button[contains(@class,'waiting') and contains(., 'Review 1 change')]")), 10000).click()
  await driver.wait(until.elementLocated(By.xpath("//div[contains(@class,'dl') and contains(@class,'add') and contains(., 'red folder')]")), 10000)
  assert.match(await driver.findElement(By.xpath("//div[contains(@class,'dl') and contains(@class,'del')]")).getText(), /blue folder/)
  await driver.findElement(By.xpath("//button[normalize-space(.)='Approve']")).click()
  await driver.wait(until.elementLocated(By.xpath("//div[@class='md']//p[contains(., 'red folder')]")), 10000)
  const docLog = await driver.executeAsyncScript(`
    const done = arguments[arguments.length - 1]
    const call = (cmd, args) => window.__TAURI_INTERNALS__.invoke(cmd, args)
    call('doc_history', { channelId: ${JSON.stringify(intakeId)}, path: ${JSON.stringify(doc.path)} }).then(h => done(h.map(c => c.author + ': ' + c.message)), e => done({ error: String(e) }))
  `)
  assert.equal(docLog.error, undefined, docLog.error)
  assert.deepEqual(docLog, ["Demo: Approve intake's change to Intake SOP: The folder is red now", 'Demo: Edit Intake SOP', 'Demo: Add doc Intake SOP'])
  console.log('doc history:', JSON.stringify(docLog))

  // @mentions wake the agent: the command from setup runs with the mention
  // on stdin, and it answers in the channel through the MCP endpoint.
  await driver.findElement(By.css('#composer')).sendKeys('@intake where do the forms go?', Key.ENTER)
  await driver.wait(until.elementLocated(By.xpath("//span[@class='mention' and .='@intake']")), 10000)
  const reply = await driver.wait(until.elementLocated(By.xpath("//div[@class='msg'][.//span[@class='author' and .='intake']]//span[@class='text' and contains(., 'red folder')]")), 30000)
  assert.match(await reply.getText(), /I saw Northside sent three more\./, 'the conversation came with the mention')
  console.log('agent answered:', await reply.getText())
  const woken = await driver.executeAsyncScript(`
    const done = arguments[arguments.length - 1]
    window.__TAURI_INTERNALS__.invoke('activity', { channelId: ${JSON.stringify(intakeId)} }).then(a => done(a.filter(x => x.action === 'woken').map(x => x.agent + ': ' + x.detail + ' ' + x.ok)), e => done({ error: String(e) }))
  `)
  assert.deepEqual(woken, ['intake: woken by Demo true'])

  // Removing it, from its settings: gone from the sidebar, and its token
  // refused.
  await driver.findElement(By.xpath("//button[contains(@class,'agentrow') and contains(., 'intake')]")).click()
  await driver.wait(until.elementLocated(By.xpath("//button[normalize-space(.)='Remove agent']")), 10000).click()
  await driver.findElement(By.xpath("//button[contains(@class,'danger') and normalize-space(.)='Remove intake']")).click()
  await driver.wait(async () => (await driver.findElements(By.css('button.agentrow'))).length === 0, 10000)
  const after = await mcp({ jsonrpc: '2.0', id: 9, method: 'tools/call', params: { name: 'list_channels', arguments: {} } }, init.session)
  assert.equal(after.json.result.isError, true)
  assert.match(after.json.result.content[0].text, /no valid agent token/, 'the removed agent is refused')
  console.log('removed: intake')

  // Export from the window: a folder of open files, and the history clones.
  await driver.findElement(By.xpath("//button[contains(., 'Export workspace')]")).click()
  const outcome = await driver.wait(until.elementLocated(By.xpath("//p[contains(., 'Saved to')] | //div[contains(@class,'exportbox')]//p[contains(@class,'error')]")), 20000)
  assert.match(await outcome.getText(), /Saved to/)
  const out = await driver.executeAsyncScript(`
    const done = arguments[arguments.length - 1]
    const call = (cmd, args) => window.__TAURI_INTERNALS__.invoke(cmd, args)
    call('workspaces').then(ws => call('export_workspace', { workspaceId: ws[0].id })).then(done, e => done({ error: String(e) }))
  `)
  assert.equal(out.error, undefined, out.error)
  const dir = `${out}/channels/student-intake`
  assert.equal(readFileSync(`${dir}/table.csv`, 'utf8').split('\n')[1], 'Jordan Lee,Northside,,Docs needed')
  assert.match(readFileSync(`${dir}/chat.md`, 'utf8'), /Northside sent three more students\./)
  assert.ok(existsSync(`${dir}/history.git/HEAD`), 'the history is a git repository')
  assert.match(readFileSync(`${dir}/docs/intake-sop.md`, 'utf8'), /red folder/)
  assert.ok(existsSync(`${out}/export-viewer.html`))
  console.log('exported:', out)

  // Import that export back (the folder picker is the system's; the window
  // calls this same command with the folder you pick), then check it in the
  // window and through the core, and export it again: the same files.
  const imported = await driver.executeAsyncScript(`
    const done = arguments[arguments.length - 1]
    const call = (cmd, args) => window.__TAURI_INTERNALS__.invoke(cmd, args)
    ;(async () => {
      const ws = await call('import_workspace', { folder: ${JSON.stringify(out)} })
      const chans = await call('channels', { workspaceId: ws.id })
      const intake = chans.find(c => c.name === 'student-intake')
      const docs = await call('channel_docs', { channelId: intake.id })
      const again = await call('export_workspace', { workspaceId: ws.id })
      return { ws, channels: chans.map(c => c.name), doc: docs.docs[0].body, messages: (await call('messages', { channelId: intake.id })).length, again }
    })().then(done, e => done({ error: String(e) }))
  `)
  assert.equal(imported.error, undefined, imported.error)
  assert.deepEqual(imported.channels, ['general', 'student-intake'])
  assert.match(imported.doc, /red folder/)
  const before = readFileSync(`${dir}/chat.jsonl`, 'utf8').trim().split('\n').length
  assert.equal(imported.messages, before, 'every message came back')
  const dir2 = `${imported.again}/channels/student-intake`
  assert.equal(readFileSync(`${dir2}/table.csv`, 'utf8'), readFileSync(`${dir}/table.csv`, 'utf8'))
  assert.equal(readFileSync(`${dir2}/docs/intake-sop.md`, 'utf8'), readFileSync(`${dir}/docs/intake-sop.md`, 'utf8'))
  const tip = d => { const head = readFileSync(`${d}/history.git/HEAD`, 'utf8').trim().replace(/^ref: /, ''); return readFileSync(`${d}/history.git/${head}`, 'utf8').trim() }
  assert.match(tip(dir), /^[0-9a-f]{40}$/)
  assert.equal(tip(dir2), tip(dir), 'the same history, commit for commit')
  await driver.findElement(By.xpath("//button[contains(., 'All workspaces')]")).click()
  const cards = await driver.wait(until.elementsLocated(By.css('button.card')), 10000)
  assert.equal(cards.length, 2, 'the imported workspace is on the home screen')
  assert.ok(await driver.findElement(By.xpath("//button[normalize-space(.)='Import a workspace']")).isDisplayed())
  console.log('imported:', JSON.stringify({ channels: imported.channels, messages: imported.messages }))
} finally {
  await driver.quit()
}
