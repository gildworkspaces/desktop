// The built app, driven through tauri-driver (Linux CI, under Xvfb): the
// window opens on the seeded demo workspace, and what you type reaches the
// Rust core and comes back from its database.
//   tauri-driver &   node tests/e2e/smoke.mjs <path to the built binary>
import { Builder, By, until } from 'selenium-webdriver'
import assert from 'node:assert/strict'

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
  await driver.findElement(By.css('#new-channel')).sendKeys('Student Intake\n')
  await driver.wait(until.elementLocated(By.xpath("//h2[contains(., 'student-intake')]")), 10000)
  await driver.findElement(By.css('#composer')).sendKeys('Northside sent three more students.\n')
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
  await driver.findElement(By.css('#rec-title')).sendKeys('Jordan Lee\n')
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
} finally {
  await driver.quit()
}
