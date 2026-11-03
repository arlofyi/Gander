/**
 * Data and cache layer (WP-22, WP-23): Room tables (`hosts`, `shares`, `entries`, `cached_files`),
 * the Keystore-wrapped token store on DataStore, settings, and the LRU file cache.
 *
 * Everything persisted here is excluded from backup and device transfer by
 * `res/xml/data_extraction_rules.xml`; keep it that way when adding new storage locations.
 */
package fyi.arlo.gander.core.data
