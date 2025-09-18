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

## Submission
- [ ] Add bot submission system via file upload on web interface
- [ ] Add validation of bot submissions to make sure they are a real tournament participant (e.g. student id or similar
- [ ] Add validation of bot submissions to make sure they adhere to api
- [ ] Implement player submission management system ensuring only one bot active at a time, each submission has submission id identifying both the player and their submission number

## Testing
- [ ] Add unit tests for poker hand evaluation
- [ ] Validate blind progression and table breaking logic
- [ ] Test process-based bot communication

## Production Features
- [ ] Bot process management (spawn, monitor, restart)
- [ ] Tournament logging and metrics
- [ ] Configuration validation

