# TODO

## Core Implementation
- [ ] Complete table-runner integration with tournament-core
- [ ] Implement missing GameRunner methods (deal cards, betting rounds, showdown)
- [ ] Add ProcessBot stdin/stdout JSON communication
- [ ] Wire up event flow between tables and tournament director

## Testing & Validation
- [ ] Add unit tests for poker hand evaluation
- [ ] Test tournament flow with mock bots
- [ ] Validate blind progression and table breaking logic
- [ ] Test process-based bot communication

## Production Features
- [ ] Bot process management (spawn, monitor, restart)
- [ ] Tournament logging and metrics
- [ ] Configuration validation
- [ ] Error handling for bot disconnections

## Future Enhancements
- [ ] Multi-table tournament UI/dashboard
- [ ] Tournament replay system
- [ ] Advanced blind structures (antes, bounties)
- [ ] Bot performance analytics