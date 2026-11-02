/*
 * retifs Map's shared core, as C (haiku/capi): JSON in and out, a map's
 * tiles as draw lists. Strings are freed with rm_free(), byte buffers with
 * rm_free_bytes(). rm_load() reads the places and maps; the other calls name
 * places by number ("i") and maps by ID. Answers that can fail are objects
 * with "error" when they did. Calls block.
 *
 * A tile's draw list (little-endian), item after item, in tile units (0..1
 * across the tile): a kind byte, then
 *   0 fill:  u32 color, u32 outline (0: none), parts
 *   1 line:  u32 color, f32 width, u8 dash count, f32 dashes…, parts
 *   2 label: u32 color, u32 halo, f32 halo width, f32 size, f32 x, f32 y,
 *            f32 angle (degrees, y down), u16 length, UTF-8 text
 * parts: u32 count, then per part u32 point count and f32 x, y pairs.
 * Colors are 0xAARRGGBB.
 */
#ifndef RETIFS_MAP_H
#define RETIFS_MAP_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

char* rm_load(void);
/* The libraries to read one by one: {"libraries", "lazy", "address"}. */
char* rm_libraries(void);
/* One library: {"ns", "tilesets", "places", "unplaced", "by_cell", "records",
 * "peers", "lazy", "writable"}. */
char* rm_library(const char* ns, int lazy);
char* rm_places_in(const char* ns, double west, double south, double east,
	double north, int zoom);
/* {"background"} or {"error"}: read once before the map's tiles. */
char* rm_style(const char* tileset);
uint8_t* rm_tile(const char* tileset, int z, int x, int y, size_t* len);
uint8_t* rm_thumb(int place, size_t* len);
char* rm_details(const char* ns, const char* id);
char* rm_find(const char* text);
char* rm_save_place(const char* ns, const char* name, double lat, double lon,
	const char* address);
char* rm_place(const char* ns, const char* id, const char* key, double lat,
	double lon);
char* rm_heard(void);
char* rm_join(const char* address);
char* rm_add_layer(const char* link, const char* name);
char* rm_leave(const char* ns);
char* rm_change_counts(void);

void rm_free(char* s);
void rm_free_bytes(uint8_t* bytes, size_t len);

#ifdef __cplusplus
}
#endif

#endif
