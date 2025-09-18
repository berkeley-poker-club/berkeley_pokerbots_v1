# Tournament Core Integration Analysis

This document analyzes what updates are needed to integrate the new `poker-utils` and `table-runner` crates with the existing `tournament-core`.

## Current State Analysis

### What Tournament-Core Has
- Tournament orchestration logic in `runner.rs`
- Configuration management in `config.rs`
- Abstract table management in `table_manager.rs`
- Basic player ID types in `player_manager.rs`

### What's Now Available
- **poker-utils**: Complete poker game engine with cards, hands, betting, pots
- **table-runner**: Concrete table implementation with game execution
- **Concrete TableHandle**: Actual implementation of the abstract trait

## Required Integration Changes

### 1. Import and Dependency Updates

**Files Affected**: `tournament-core/Cargo.toml`, `tournament-core/src/lib.rs`

**Changes Needed**:
- Dependencies already added to Cargo.toml ✓
- Need to import concrete table types from table-runner
- Need to import poker game types from poker-utils

### 2. Table Manager Updates

**File**: `tournament-core/src/table_manager.rs`

**Current Issues**:
- `TableEvent` enum has wrong field types:
  - `TableSizes { active_count: PlayerId }` should be `usize`
  - `ReadyForReseat { open_seats: PlayerId }` should be `usize`
  - `LevelApplied { level_id: PlayerId }` should be `u32`
- Missing import of concrete table implementation
- No factory method to create actual tables

**Required Changes**:
- Fix field types in TableEvent enum
- Import `PokerTable` (or whatever we rename ConcreteTable to)
- Add table creation factory methods
- Update TableHandle trait bounds if needed

### 3. Tournament Runner Integration

**File**: `tournament-core/src/runner.rs`

**Current Issues**:
- Uses generic `H: TableHandle` but needs concrete implementation
- No actual table creation - just uses abstract interface
- Missing integration with poker game rules
- Event processing needs to handle real table events

**Required Changes**:
- Replace generic table handling with concrete table creation
- Integrate `GameRules` from poker-utils with `TournamentConfig`
- Update table creation to use actual table-runner implementation
- Handle real table events (player eliminations, hand completions)
- Connect tournament blind levels to actual game rules

### 4. Configuration Integration

**File**: `tournament-core/src/config.rs`

**Current Issues**:
- `LevelSpec` duplicated between tournament-core and poker-utils
- No integration between tournament config and game rules
- Missing player stack management configuration

**Required Changes**:
- Remove duplicate `LevelSpec` - use the one from poker-utils
- Add conversion methods between `TournamentConfig` and `GameRules`
- Add initial stack sizes, table capacity settings
- Integrate ante progression with blind levels

### 5. Player Management Enhancement

**File**: `tournament-core/src/player_manager.rs`

**Current Issues**:
- Only defines `PlayerId` type alias
- No actual player management or bot integration
- Missing player lifecycle management

**Required Changes**:
- Import player traits from table-runner
- Add player registry and bot management
- Handle player connections, disconnections, timeouts
- Integrate with actual player implementations

### 6. Event Processing Updates

**Throughout tournament-core**

**Current Issues**:
- Event processing is abstract/placeholder
- No integration with real game events
- Missing player elimination detection
- No hand completion tracking

**Required Changes**:
- Process real `TableEvent`s from concrete tables
- Track actual hand counts per player
- Handle player eliminations and stack updates
- Update table sizes based on real game state

## Specific Code Changes Required

### TableEvent Type Fixes
```rust
// Current (incorrect)
TableSizes { table_id: TableId, active_count: PlayerId }

// Should be
TableSizes { table_id: TableId, active_count: usize }
```

### Table Creation Integration
```rust
// Need to add to runner.rs
use table_runner::PokerTable;
use poker_utils::GameRules;

// Replace generic table creation with concrete implementation
fn create_table(&self, capacity: usize) -> PokerTable {
    let rules = GameRules {
        small_blind: self.cfg.blind_levels[0].small_blind,
        big_blind: self.cfg.blind_levels[0].big_blind,
        ante: self.cfg.blind_levels[0].ante,
        max_seats: capacity,
    };
    PokerTable::new(table_id, capacity, rules)
}
```

### Configuration Unification
```rust
// Remove duplicate LevelSpec from tournament-core
// Use poker_utils::LevelSpec instead
use poker_utils::LevelSpec;

// Add conversion methods
impl TournamentConfig {
    fn to_game_rules(&self, level_index: usize) -> GameRules {
        let level = &self.blind_levels[level_index];
        GameRules {
            small_blind: level.small_blind,
            big_blind: level.big_blind,
            ante: level.ante,
            max_seats: self.table_size,
        }
    }
}
```

### Player Integration
```rust
// Add to player_manager.rs
use table_runner::{Player, MockPlayer};

pub struct PlayerRegistry {
    active_players: HashMap<PlayerId, Box<dyn Player>>,
    // ... player management logic
}
```

## Testing Integration Points

### Unit Tests Needed
1. **Table Creation**: Verify concrete tables are created with correct rules
2. **Event Processing**: Test real table event handling
3. **Configuration Conversion**: Test tournament config → game rules conversion
4. **Player Management**: Test player seating and elimination

### Integration Tests Needed
1. **Full Tournament Flow**: End-to-end tournament with real tables
2. **Multi-Table Management**: Table breaking and rebalancing with concrete tables
3. **Blind Progression**: Verify blind levels are applied to actual games
4. **Player Elimination**: Test elimination detection and ranking

## Migration Strategy

### Phase 1: Type System Integration
1. Fix TableEvent field types
2. Import concrete table types
3. Update configuration to remove duplicates
4. Ensure clean compilation

### Phase 2: Table Creation
1. Replace abstract table handling with concrete implementation
2. Integrate game rules with tournament configuration
3. Test table creation and basic operation

### Phase 3: Event Processing
1. Process real table events
2. Handle player eliminations and stack updates
3. Track hand counts and table sizes

### Phase 4: Full Integration Testing
1. End-to-end tournament execution
2. Multi-table scenarios
3. Error handling and edge cases

## Potential Challenges

### 1. Type System Complexity
- Multiple similar types (PlayerId, SeatIndex, TableId)
- Need careful type conversions between crates
- Async/sync boundary management

### 2. State Synchronization
- Tournament state vs table state consistency
- Player stack management across table moves
- Hand count tracking accuracy

### 3. Error Handling
- Table errors vs tournament errors
- Player timeout/disconnection handling
- Graceful degradation on table failures

### 4. Performance Considerations
- Multiple async table tasks
- Event processing overhead
- Memory management for large tournaments

## Success Criteria

The integration is successful when:
1. ✅ All code compiles without warnings
2. ✅ Unit tests pass for all integration points
3. ✅ Can create and run concrete poker tables
4. ✅ Tournament can manage multiple real tables
5. ✅ Player eliminations are detected and tracked
6. ✅ Blind progression works with actual games
7. ✅ Table breaking/rebalancing works with concrete tables
8. ✅ End-to-end tournament completes successfully

This integration will transform the tournament system from an abstract framework into a fully functional poker tournament engine.