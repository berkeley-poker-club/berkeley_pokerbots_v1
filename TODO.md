# TODO

## Core Implementation ✅ COMPLETED
- [x] Complete table-runner integration with tournament-core
- [x] Implement missing GameRunner methods (deal cards, betting rounds, showdown)
- [x] Add ProcessBot stdin/stdout JSON communication
- [x] Wire up event flow between tables and tournament director
- [x] Implement waiting queue system for player seating
- [x] Add functional blind generation (unlimited tournament duration)
- [x] Integrate PlayerRegistry with shared state management
- [x] Fix circular dependency and import issues
- [x] **Table Breaking & Bot Migration System**
  - [x] Shared ownership model for ProcessBot instances (Arc<Mutex<ProcessBot>>)
  - [x] Player state tracking (Available, Playing, InTransit, WaitingQueue, Disconnected)
  - [x] Table extraction commands (ExtractPlayer, ExtractAllPlayers, FinishHandAndExtract)
  - [x] Migration events (PlayerExtracted, ExtractionFailed, AllPlayersExtracted)
  - [x] Tournament director migration orchestration with rollback support
  - [x] Graceful table breaking without losing bot processes

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