// No packages required: node tests/hand-viewer.test.cjs
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const source = fs.readFileSync(require('node:path').join(__dirname, '../hand-viewer.html'), 'utf8').split('<script>')[1].split('</script>')[0];
const context = {};
vm.runInNewContext(source.slice(0, source.indexOf('let lanes =')) + '\nglobalThis.test = { calculateStats, statValue, parseHistory, replay, replayEvents };', context);
const { calculateStats, statValue, parseHistory, replay, replayEvents } = context.test;
let checks = 0;
function check(name, fn) { fn(); checks++; console.log(`PASS ${name}`); }
const action = (seat, street, kind, amount = 0, committed = amount, all_in = false) => ({ kind: 'ActionTaken', seat, street, action: { kind, amount: committed }, amount, committed_street: committed, all_in });
const board = street => ({ kind: 'BoardDealt', street, board: ['Ah', 'Kd', '2c'], cards: [], pot: 0 });
function hand(events, { players = 3, table = 1, button = 0, bb = 10, finals = {}, stacks = {}, ids = {}, showdown = false, wins = [] } = {}) {
  const seats = Array.from({ length: players }, (_, seat) => ({ seat, player_id: ids[seat] ?? seat + 1, stack: stacks[seat] ?? 1000, committed_street: 0, committed_total: 0, status: 'Active' }));
  const final = seats.map(s => ({ ...s, stack: finals[s.seat] ?? s.stack }));
  return { tournament_id: 'test', table_id: table, hand_id: table * 2 ** 32 + 1, level: 0,
    events: [{ kind: 'HandStarted', table_id: table, hand_id: table * 2 ** 32 + 1, button, big_blind: bb, small_blind: bb / 2, seats }, ...events,
      ...wins.map(([seat, amount]) => ({ kind: 'PotAwarded', pot_index: 0, amount, winners: [[seat, amount]] })), { kind: 'HandEnded', seats: final }],
    result: { seats: final, went_to_showdown: showdown, awards: [] } };
}
check('forced blinds and a free check do not count as VPIP or PFR', () => {
  const stats = calculateStats([hand([{ kind: 'BlindPosted', seat: 0, amount: 10, big: true }, action(0, 'Preflop', 'Check')])]);
  assert.equal(stats.get('1').vpip, 0); assert.equal(stats.get('1').pfr, 0);
  assert.equal(statValue(stats.get('1'), 'threeBet'), null);
});
check('multiple raises in one hand count once for PFR; 3B uses opportunities', () => {
  const stats = calculateStats([hand([action(0, 'Preflop', 'RaiseTo', 30), action(1, 'Preflop', 'Call', 30), action(2, 'Preflop', 'RaiseTo', 90), action(0, 'Preflop', 'RaiseTo', 170, 200), action(1, 'Preflop', 'Fold', 0, 30)])]);
  assert.equal(stats.get('1').pfr, 1); assert.equal(stats.get('3').threeBet, 1);
  assert.equal(stats.get('2').threeBetOpp, 1); assert.equal(stats.get('2').threeBet, 0);
  assert.equal(stats.get('1').foldThreeBetOpp, 1);
  assert.equal(stats.get('2').foldThreeBetOpp, 0, 'facing a 4-bet is not a fold-to-3-bet opportunity');
});
check('short all-in raises do not reopen a previous caller’s 3B opportunity', () => {
  const stats = calculateStats([hand([action(0, 'Preflop', 'Call', 10), action(1, 'Preflop', 'RaiseTo', 15, 15, true), action(2, 'Preflop', 'Call', 15), action(0, 'Preflop', 'Call', 5, 15)], { stacks: { 1: 15 } })]);
  assert.equal(stats.get('1').threeBetOpp, 0); assert.equal(stats.get('3').threeBetOpp, 1);
});
check('flop/turn/river CB, fold-to-CB, AF and AFq use actual decisions', () => {
  const stats = calculateStats([hand([action(0, 'Preflop', 'RaiseTo', 30), action(1, 'Preflop', 'Call', 30), action(2, 'Preflop', 'Call', 30), board('Flop'),
    action(1, 'Flop', 'Check'), action(2, 'Flop', 'Check'), action(0, 'Flop', 'BetTo', 40), action(1, 'Flop', 'Call', 40), action(2, 'Flop', 'Fold'),
    board('Turn'), action(1, 'Turn', 'Check'), action(0, 'Turn', 'BetTo', 80), action(1, 'Turn', 'Call', 80),
    board('River'), action(1, 'River', 'Check'), action(0, 'River', 'BetTo', 100), action(1, 'River', 'Call', 100)], { showdown: true, wins: [[0, 300]] })]);
  const raiser = stats.get('1'), caller = stats.get('2'), folder = stats.get('3');
  for (const key of ['cbet', 'turnCbet', 'riverCbet']) { assert.equal(raiser[key], 1); assert.equal(raiser[key + 'Opp'], 1); }
  assert.equal(folder.foldCbet, 1); assert.equal(caller.foldCbetOpp, 1);
  assert.equal(statValue(raiser, 'af'), Infinity); assert.equal(statValue(caller, 'af'), 0);
  assert.equal(statValue(raiser, 'afq'), 100); assert.equal(raiser.showdowns, 1); assert.equal(folder.showdowns, 0);
});
check('a donk bet removes the preflop raiser’s c-bet opportunity', () => {
  const stats = calculateStats([hand([action(0, 'Preflop', 'RaiseTo', 30), action(1, 'Preflop', 'Call', 30), board('Flop'), action(1, 'Flop', 'BetTo', 40), action(0, 'Flop', 'RaiseTo', 100)])]);
  assert.equal(stats.get('1').cbetOpp, 0); assert.equal(stats.get('1').cbet, 0); assert.equal(stats.get('2').cbet, 0);
});
check('checking the flop breaks the continuation-bet sequence', () => {
  const stats = calculateStats([hand([action(0, 'Preflop', 'RaiseTo', 30), board('Flop'), action(0, 'Flop', 'Check'), board('Turn'), action(0, 'Turn', 'BetTo', 40)])]);
  assert.equal(stats.get('1').cbetOpp, 1); assert.equal(stats.get('1').cbet, 0); assert.equal(stats.get('1').turnCbetOpp, 0);
});
check('multiway raises stop fold-to-c-bet and later c-bet opportunities', () => {
  const stats = calculateStats([hand([action(0, 'Preflop', 'RaiseTo', 30), board('Flop'), action(0, 'Flop', 'BetTo', 40), action(1, 'Flop', 'RaiseTo', 120), action(2, 'Flop', 'Fold'), board('Turn'), action(0, 'Turn', 'BetTo', 100)])]);
  assert.equal(stats.get('2').foldCbetOpp, 1); assert.equal(stats.get('3').foldCbetOpp, 0);
  assert.equal(stats.get('1').turnCbetOpp, 0);
});
check('all-in runouts count saw-flop/showdown but no c-bet opportunity; split/side pots count as wins', () => {
  const stats = calculateStats([hand([action(0, 'Preflop', 'RaiseTo', 1000, 1000, true), action(1, 'Preflop', 'Call', 1000, 1000, true), action(2, 'Preflop', 'Fold'), board('Flop'), board('Turn'), board('River')], { showdown: true, wins: [[0, 1000], [1, 1000]] })]);
  assert.equal(stats.get('1').cbetOpp, 0); assert.equal(stats.get('1').sawFlop, 1); assert.equal(statValue(stats.get('1'), 'wtsd'), 100);
  assert.equal(stats.get('1').showdownWins, 1); assert.equal(stats.get('2').showdownWins, 1); assert.equal(stats.get('3').sawFlop, 0);
});
check('steal positions exclude the big blind at a three-handed table', () => {
  const stats = calculateStats([hand([action(0, 'Preflop', 'Fold'), action(1, 'Preflop', 'Fold'), action(2, 'Preflop', 'Check')])]);
  assert.equal(stats.get('1').stealOpp, 1); assert.equal(stats.get('2').stealOpp, 1); assert.equal(stats.get('3').stealOpp, 0);
});
check('players retain identity across tables; chip rate uses each hand’s blind', () => {
  const a = hand([], { finals: { 0: 1100 }, bb: 10 });
  const b = hand([], { table: 2, ids: { 0: 4, 1: 1 }, stacks: { 1: 1100 }, finals: { 1: 1000 }, bb: 20 });
  const stats = calculateStats([a, b]), player = stats.get('1');
  assert.equal(player.hands, 2); assert.equal(player.net, 0); assert.equal(player.latestTable, 2); assert.equal(player.latestStack, 1000);
  assert.equal(statValue(player, 'bb100'), 250);
});
if (fs.existsSync(require('node:path').join(__dirname, '../hands.jsonl'))) check('real history replays conserve chips through every step and match final stacks', () => {
  const parsed = parseHistory(fs.readFileSync(require('node:path').join(__dirname, '../hands.jsonl'), 'utf8'));
  assert.equal(parsed.errors.length, 0); assert(parsed.records.length > 0);
  for (const r of parsed.records) {
    const total = r.events.find(e => e.kind === 'HandStarted').seats.reduce((n, s) => n + s.stack, 0), events = replayEvents(r);
    for (let i = 0; i < events.length; i++) {
      const state = replay(r, i);
      assert.equal([...state.seats.values()].reduce((n, s) => n + s.stack, state.pot), total);
    }
    const final = replay(r, events.length - 1);
    for (const s of r.result.seats) assert.equal(final.seats.get(s.seat).stack, s.stack);
  }
});
console.log(`${checks} hand-viewer checks passed.`);
