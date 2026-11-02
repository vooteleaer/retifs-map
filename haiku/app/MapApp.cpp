/*
 * retifs Map for Haiku, with the Haiku API: the Linux app's layout (a
 * sidebar with the maps and layers, a bar with add, search and the
 * announces, the map) over the shared core (retifs_map.h). The map is drawn
 * here: each tile's draw list (the map library's own MapLibre style applied
 * by the core) filled and stroked with BShapes into a bitmap off the window
 * thread; labels over the whole view, so they don't collide across tiles.
 * Tiles come from map libraries in retifs only, never from the internet.
 */

#include <Application.h>
#include <Bitmap.h>
#include <DataIO.h>
#include <TranslationUtils.h>
#include <Button.h>
#include <CheckBox.h>
#include <GroupView.h>
#include <LayoutBuilder.h>
#include <MessageRunner.h>
#include <RadioButton.h>
#include <Screen.h>
#include <ScrollView.h>
#include <SeparatorView.h>
#include <SpaceLayoutItem.h>
#include <StatusBar.h>
#include <Alert.h>
#include <Invoker.h>
#include <MenuItem.h>
#include <MessageFilter.h>
#include <PopUpMenu.h>
#include <Shape.h>
#include <StringView.h>
#include <TextControl.h>
#include <Window.h>

#include <Json.h>

#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <atomic>
#include <list>
#include <map>
#include <set>
#include <string>
#include <vector>

#include "retifs_map.h"

static const char* kSignature = "application/x-vnd.retifs-map";

enum {
	kLoad = 'load',			// worker: read everything
	kLoaded = 'ldd_',		// window: what it read
	kStyle = 'styl',		// worker: read a map's style
	kStyled = 'stld',
	kTile = 'tile',			// worker: draw a tile
	kTileDone = 'tdon',
	kPickMap = 'pmap',
	kLibraries = 'libs',	// window: a library shown or hidden
	kZoomIn = 'zmin',
	kZoomOut = 'zmou',
	kChanges = 'chng',
	kChanged = 'chgd',
	kAnnounces = 'annc',	// window: open the announces
	kHeard = 'herd',		// worker: the libraries heard ("reply": where to)
	kHeardDone = 'hrdd',
	kJoinHeard = 'jhrd',	// worker: join one ("address")
	kJoined = 'jond',
	kReload = 'rlod',		// window: read everything again
	kLoadStart = 'lsta',	// window: the libraries to read
	kLoadStep = 'lstp',		// window: reading this one now
	kLoadLibrary = 'llib',	// window: one read
	kLoadDone = 'ldon',
	kToggle = 'togl',		// window: show or hide a library ("ns")
	kLeaveAsk = 'leva',		// window: leave a library? ("ns")
	kLeave = 'leav',		// worker: leave it ("ns")
	kLeft = 'left',			// window: left ("result")
	kThumb = 'thmb',		// tile worker: a photo's thumbnail ("i")
	kThumbDone = 'thmd',
	kPreview = 'prev',		// tile worker: a photo's preview ("i", "reply")
	kPreviewDone = 'prvd',
	kDetails = 'detl',		// tile worker: a record's attributes ("ns", "id", "reply")
	kDetailsDone = 'dtld',
	kCloseBubble = 'clbb',
	kOpenPhoto = 'ophp',
	kSearch = 'srch',		// window: search what's typed
	kFind = 'find',			// worker: search ("text")
	kFound = 'fnd_',		// window: what was found
	kPick = 'pick',			// window: one picked ("lat", "lon", "zoom", "place")
	kViewChanged = 'vchg',	// window: the map moved or zoomed
	kPlacesIn = 'plin',		// worker: a lazy library's places in an area
	kPlacesFound = 'plfd',
	kKeep = 'keep',			// worker: keep all here or not ("ns", "all")
	kKept = 'kept',			// window: done ("result")
};

static const int kTileSize = 256;
static const int kMaxZoom = 18;
/// Markers closer than this (pixels) at the current zoom become one (as on Linux).
static const double kCell = 56.0;


// #pragma mark - JSON helpers


static BMessage
Take(char* json)
{
	BMessage m;
	if (json != NULL) {
		BJson::Parse(json, m);
		rm_free(json);
	}
	return m;
}


static int32
Count(const BMessage& array)
{
	return array.CountNames(B_ANY_TYPE);
}


static BMessage
Item(const BMessage& array, int32 i)
{
	BMessage item;
	BString name;
	name << i;
	array.FindMessage(name.String(), &item);
	return item;
}


static double
Number(const BMessage& array, int32 i)
{
	BString name;
	name << i;
	return array.GetDouble(name.String(), 0);
}


static rgb_color
Color(uint32 argb)
{
	return make_color((argb >> 16) & 0xff, (argb >> 8) & 0xff, argb & 0xff,
		argb >> 24);
}


// #pragma mark - Web Mercator


static double
WorldX(double lon, int z)
{
	return (lon + 180.0) / 360.0 * kTileSize * (double)(1 << z);
}


static double
WorldY(double lat, int z)
{
	lat = std::max(-85.05, std::min(85.05, lat));
	double phi = lat * M_PI / 180.0;
	return (1.0 - log(tan(phi) + 1.0 / cos(phi)) / M_PI) / 2.0 * kTileSize
		* (double)(1 << z);
}


static double
Lon(double x, int z)
{
	return x / (kTileSize * (double)(1 << z)) * 360.0 - 180.0;
}


static double
Lat(double y, int z)
{
	double n = M_PI - 2.0 * M_PI * y / (kTileSize * (double)(1 << z));
	return 180.0 / M_PI * atan(0.5 * (exp(n) - exp(-n)));
}


// #pragma mark - Images


/*! An image's bytes (JPEG, PNG…) as a bitmap, by the Translation Kit. */
static BBitmap*
Decode(const uint8* data, size_t length)
{
	if (data == NULL || length == 0)
		return NULL;
	BMemoryIO io(data, length);
	BBitmap* bitmap = BTranslationUtils::GetBitmap(&io);
	if (bitmap == NULL || !bitmap->IsValid()) {
		delete bitmap;
		return NULL;
	}
	if (bitmap->ColorSpace() != B_RGBA32 && bitmap->ColorSpace() != B_RGB32) {
		BBitmap* rgba = new BBitmap(bitmap->Bounds(), B_RGBA32);
		rgba->ImportBits(bitmap);
		delete bitmap;
		bitmap = rgba;
	}
	return bitmap;
}


/*! `source` (or its middle square, if `square`) scaled down to fit
	width×height, each pixel the average of those it covers. */
static BBitmap*
Scaled(const BBitmap* source, int width, int height, bool square)
{
	int sw = source->Bounds().IntegerWidth() + 1;
	int sh = source->Bounds().IntegerHeight() + 1;
	int sx0 = 0, sy0 = 0;
	if (square) {
		int side = std::min(sw, sh);
		sx0 = (sw - side) / 2;
		sy0 = (sh - side) / 2;
		sw = sh = side;
	}
	double scale = std::min(1.0, std::min((double)width / sw, (double)height / sh));
	int dw = std::max(1, (int)(sw * scale)), dh = std::max(1, (int)(sh * scale));
	BBitmap* out = new BBitmap(BRect(0, 0, dw - 1, dh - 1), B_RGBA32);
	const uint8* in = (const uint8*)source->Bits();
	int inRow = source->BytesPerRow();
	bool alpha = source->ColorSpace() == B_RGBA32;
	uint8* to = (uint8*)out->Bits();
	int outRow = out->BytesPerRow();
	for (int y = 0; y < dh; y++) {
		int ya = sy0 + y * sh / dh, yb = std::max(ya + 1, sy0 + (y + 1) * sh / dh);
		for (int x = 0; x < dw; x++) {
			int xa = sx0 + x * sw / dw, xb = std::max(xa + 1, sx0 + (x + 1) * sw / dw);
			uint32 sum[4] = { 0, 0, 0, 0 }, n = 0;
			for (int yy = ya; yy < yb; yy++) {
				const uint8* p = in + yy * inRow + xa * 4;
				for (int xx = xa; xx < xb; xx++, p += 4, n++) {
					sum[0] += p[0];
					sum[1] += p[1];
					sum[2] += p[2];
					sum[3] += alpha ? p[3] : 255;
				}
			}
			uint8* q = to + y * outRow + x * 4;
			for (int c = 0; c < 4; c++)
				q[c] = sum[c] / n;
		}
	}
	return out;
}


// #pragma mark - Tiles


struct Label {
	BString text;
	float x, y, angle, size, haloWidth;
	rgb_color color, halo;
};


struct TileImage {
	BBitmap* bitmap;
	std::vector<Label> labels;
	uint64 used;
};


static uint64
TileKey(int z, int x, int y)
{
	return ((uint64)z << 56) | ((uint64)(uint32)x << 28) | (uint64)(uint32)y;
}


/*! Reads a draw list (retifs_map.h). */
class Reader {
public:
	Reader(const uint8* data, size_t length)
		: fData(data), fLength(length), fPos(0) {}

	bool More() const { return fPos < fLength; }
	bool Ok() const { return fPos <= fLength; }

	uint8 U8() { return fPos + 1 <= fLength ? fData[fPos++] : (fPos++, 0); }

	uint32 U32()
	{
		uint32 v = 0;
		if (fPos + 4 <= fLength)
			memcpy(&v, fData + fPos, 4);
		fPos += 4;
		return v;
	}

	float F32()
	{
		float v = 0;
		if (fPos + 4 <= fLength)
			memcpy(&v, fData + fPos, 4);
		fPos += 4;
		return v;
	}

	BString Text()
	{
		uint16 n = 0;
		if (fPos + 2 <= fLength)
			memcpy(&n, fData + fPos, 2);
		fPos += 2;
		BString s;
		if (fPos + n <= fLength)
			s.SetTo((const char*)fData + fPos, n);
		fPos += n;
		return s;
	}

	/*! Parts as polylines, in pixels of a tile. */
	std::vector<std::vector<BPoint> > Parts()
	{
		std::vector<std::vector<BPoint> > parts;
		uint32 count = U32();
		for (uint32 i = 0; i < count && Ok(); i++) {
			uint32 points = U32();
			std::vector<BPoint> part;
			part.reserve(points);
			for (uint32 j = 0; j < points && Ok(); j++) {
				float x = F32();
				float y = F32();
				part.push_back(BPoint(x * kTileSize, y * kTileSize));
			}
			parts.push_back(part);
		}
		return parts;
	}

private:
	const uint8* fData;
	size_t fLength;
	size_t fPos;
};


/*! A line as dashes (BView has no dash pattern): on, off, on, … pixels. */
static void
AddDashed(BShape& shape, const std::vector<BPoint>& line,
	const std::vector<float>& dash)
{
	size_t d = 0;
	float left = dash[0];
	bool on = true;
	bool penDown = false;
	for (size_t i = 1; i < line.size(); i++) {
		BPoint a = line[i - 1], b = line[i];
		float length = sqrtf((b.x - a.x) * (b.x - a.x) + (b.y - a.y) * (b.y - a.y));
		float done = 0;
		while (length - done > 0.01f) {
			float step = std::min(left, length - done);
			BPoint from = a + BPoint((b.x - a.x) * done / length,
				(b.y - a.y) * done / length);
			BPoint to = a + BPoint((b.x - a.x) * (done + step) / length,
				(b.y - a.y) * (done + step) / length);
			if (on) {
				if (!penDown)
					shape.MoveTo(from);
				shape.LineTo(to);
				penDown = true;
			}
			done += step;
			left -= step;
			if (left <= 0.01f) {
				d = (d + 1) % dash.size();
				left = std::max(0.5f, dash[d]);
				on = !on;
				penDown = false;
			}
		}
	}
}


/*! Draws a tile's draw list into a bitmap (off the window thread). */
static TileImage
Render(const uint8* data, size_t length, rgb_color background)
{
	TileImage tile;
	tile.used = 0;
	BRect bounds(0, 0, kTileSize - 1, kTileSize - 1);
	tile.bitmap = new BBitmap(bounds, B_BITMAP_ACCEPTS_VIEWS, B_RGB32);
	BView* view = new BView(bounds, "tile", B_FOLLOW_NONE, 0);
	tile.bitmap->AddChild(view);
	tile.bitmap->Lock();
	view->SetHighColor(background);
	view->FillRect(bounds);
	view->SetDrawingMode(B_OP_ALPHA);
	view->SetBlendingMode(B_CONSTANT_ALPHA, B_ALPHA_OVERLAY);
	view->SetFillRule(B_EVEN_ODD);
	view->SetLineMode(B_ROUND_CAP, B_ROUND_JOIN);

	Reader r(data, length);
	while (r.More() && r.Ok()) {
		uint8 kind = r.U8();
		if (kind == 0) {
			rgb_color color = Color(r.U32());
			uint32 outline = r.U32();
			BShape shape;
			for (auto& part : r.Parts()) {
				if (part.size() < 3)
					continue;
				shape.MoveTo(part[0]);
				for (size_t i = 1; i < part.size(); i++)
					shape.LineTo(part[i]);
				shape.Close();
			}
			view->SetHighColor(color);
			view->FillShape(&shape);
			if (outline != 0) {
				view->SetPenSize(1);
				view->SetHighColor(Color(outline));
				view->StrokeShape(&shape);
			}
		} else if (kind == 1) {
			rgb_color color = Color(r.U32());
			float width = r.F32();
			uint8 dashes = r.U8();
			std::vector<float> dash;
			for (uint8 i = 0; i < dashes; i++)
				dash.push_back(std::max(0.5f, r.F32()));
			BShape shape;
			for (auto& part : r.Parts()) {
				if (part.size() < 2)
					continue;
				if (dash.size() >= 2) {
					AddDashed(shape, part, dash);
				} else {
					shape.MoveTo(part[0]);
					for (size_t i = 1; i < part.size(); i++)
						shape.LineTo(part[i]);
				}
			}
			view->SetPenSize(std::max(0.5f, width));
			view->SetHighColor(color);
			view->StrokeShape(&shape);
		} else if (kind == 2) {
			Label l;
			l.color = Color(r.U32());
			l.halo = Color(r.U32());
			l.haloWidth = r.F32();
			l.size = r.F32();
			l.x = r.F32();
			l.y = r.F32();
			l.angle = r.F32();
			l.text = r.Text();
			tile.labels.push_back(l);
		} else
			break;
	}
	view->Sync();
	tile.bitmap->RemoveChild(view);
	delete view;
	tile.bitmap->Unlock();
	return tile;
}


/*! Tiles are drawn by a few of these; the map's current generation (its
	zoom and map) lets them skip what the view no longer wants. */
static std::atomic<int32> sGeneration(0);


class TileWorker : public BLooper {
public:
	TileWorker(BMessenger target)
		: BLooper("map tiles", B_LOW_PRIORITY), fTarget(target) {}

	void MessageReceived(BMessage* message)
	{
		if (message->what == kThumb || message->what == kPreview) {
			int32 i = message->GetInt32("i", 0);
			size_t length = 0;
			uint8* data = message->what == kThumb ? rm_thumb(i, &length)
				: rm_preview(i, &length);
			BBitmap* image = Decode(data, length);
			if (data != NULL)
				rm_free_bytes(data, length);
			fprintf(stderr, "%s %d: %d bytes, %s\n",
				message->what == kThumb ? "thumb" : "preview", (int)i,
				(int)length, image != NULL ? "decoded" : "not decoded");
			if (image != NULL) {
				// Kept small: a camera's photo is ~48 MB decoded.
				BBitmap* small = message->what == kThumb
					? Scaled(image, 80, 80, true) : Scaled(image, 2560, 2560, false);
				delete image;
				image = small;
			}
			BMessage done(message->what == kThumb ? kThumbDone : kPreviewDone);
			done.AddInt32("i", i);
			done.AddPointer("bitmap", image);
			BMessenger reply;
			if (message->FindMessenger("reply", &reply) != B_OK)
				reply = fTarget;
			if (reply.SendMessage(&done) != B_OK)
				delete image;
			return;
		}
		if (message->what == kDetails) {
			BMessage done(kDetailsDone);
			done.AddString("id", message->GetString("id", ""));
			done.AddMessage("details", new BMessage(Take(rm_details(
				message->GetString("ns", ""), message->GetString("id", "")))));
			BMessenger reply;
			if (message->FindMessenger("reply", &reply) != B_OK)
				reply = fTarget;
			reply.SendMessage(&done);
			return;
		}
		if (message->what != kTile) {
			BLooper::MessageReceived(message);
			return;
		}
		int32 generation = message->GetInt32("gen", 0);
		BMessage done(kTileDone);
		done.AddInt32("gen", generation);
		done.AddInt32("z", message->GetInt32("z", 0));
		done.AddInt32("x", message->GetInt32("x", 0));
		done.AddInt32("y", message->GetInt32("y", 0));
		if (generation == sGeneration.load()) {
			size_t length = 0;
			bigtime_t started = system_time();
			uint8* data = rm_tile(message->GetString("map", ""),
				message->GetInt32("z", 0), message->GetInt32("x", 0),
				message->GetInt32("y", 0), &length);
			// For whoever wonders why the map is slow: on standard error.
			fprintf(stderr, "tile %d/%d/%d: %s in %d ms\n",
				(int)message->GetInt32("z", 0), (int)message->GetInt32("x", 0),
				(int)message->GetInt32("y", 0),
				data != NULL ? "drawn" : length == 1 ? "not read" : "none",
				(int)((system_time() - started) / 1000));
			if (data == NULL && length == 1) {
				// Not read now (a fetch timed out): asked again later.
				done.AddBool("failed", true);
			} else {
				rgb_color bg = Color(message->GetUInt32("background", 0xfff2efe9));
				TileImage* tile = new TileImage(Render(data,
					data != NULL ? length : 0, bg));
				if (data != NULL)
					rm_free_bytes(data, length);
				done.AddPointer("tile", tile);
			}
		}
		if (fTarget.SendMessage(&done) != B_OK) {
			TileImage* tile = NULL;
			if (done.FindPointer("tile", (void**)&tile) == B_OK && tile != NULL) {
				delete tile->bitmap;
				delete tile;
			}
		}
	}

private:
	BMessenger fTarget;
};


/*! Everything else the core is asked, one at a time. */
class Worker : public BLooper {
public:
	Worker(BMessenger target)
		: BLooper("map worker"), fTarget(target) {}

	void MessageReceived(BMessage* message)
	{
		switch (message->what) {
			case kLoad:
			{
				// One by one: the window shows each as it comes, and how far.
				BMessage list = Take(rm_libraries());
				BMessage start(kLoadStart);
				start.AddMessage("list", &list);
				fTarget.SendMessage(&start);
				if (list.HasString("error"))
					break;
				BMessage libraries, lazy;
				list.FindMessage("libraries", &libraries);
				list.FindMessage("lazy", &lazy);
				std::set<BString> lazySet;
				for (int32 i = 0; i < Count(lazy); i++)
					lazySet.insert(_Name(lazy, i));
				int32 total = Count(libraries);
				for (int32 i = 0; i < total; i++) {
					BString ns = _Name(libraries, i);
					BMessage step(kLoadStep);
					step.AddString("ns", ns);
					step.AddInt32("i", i);
					step.AddInt32("total", total);
					fTarget.SendMessage(&step);
					BMessage one(kLoadLibrary);
					one.AddMessage("data", new BMessage(Take(rm_library(ns.String(),
						lazySet.find(ns) != lazySet.end() ? 1 : 0))));
					fTarget.SendMessage(&one);
				}
				fTarget.SendMessage(kLoadDone);
				break;
			}
			case kStyle:
			{
				BMessage styled(kStyled);
				styled.AddString("map", message->GetString("map", ""));
				styled.AddMessage("style",
					new BMessage(Take(rm_style(message->GetString("map", "")))));
				fTarget.SendMessage(&styled);
				break;
			}
			case kHeard:
			{
				BMessenger reply;
				message->FindMessenger("reply", &reply);
				BMessage heard(kHeardDone);
				heard.AddMessage("heard", new BMessage(Take(rm_heard())));
				reply.SendMessage(&heard);
				break;
			}
			case kJoinHeard:
			{
				BMessenger reply;
				message->FindMessenger("reply", &reply);
				BMessage joined(kJoined);
				joined.AddString("address", message->GetString("address", ""));
				joined.AddMessage("result", new BMessage(Take(
					rm_join(message->GetString("address", "")))));
				reply.SendMessage(&joined);
				fTarget.SendMessage(kReload);
				break;
			}
			case kFind:
			{
				BMessage found(kFound);
				found.AddString("text", message->GetString("text", ""));
				found.AddMessage("found", new BMessage(Take(
					rm_find(message->GetString("text", "")))));
				fTarget.SendMessage(&found);
				break;
			}
			case kPlacesIn:
			{
				BMessage found(kPlacesFound);
				found.AddMessage("places", new BMessage(Take(rm_places_in(
					message->GetString("ns", ""), message->GetDouble("w", 0),
					message->GetDouble("s", 0), message->GetDouble("e", 0),
					message->GetDouble("n", 0), message->GetInt32("zoom", 0)))));
				fTarget.SendMessage(&found);
				break;
			}
			case kLeave:
			{
				BMessage left(kLeft);
				left.AddString("ns", message->GetString("ns", ""));
				left.AddMessage("result", new BMessage(Take(
					rm_leave(message->GetString("ns", "")))));
				fTarget.SendMessage(&left);
				break;
			}
			case kKeep:
			{
				BMessage kept(kKept);
				kept.AddString("ns", message->GetString("ns", ""));
				kept.AddMessage("result", new BMessage(Take(rm_keep(
					message->GetString("ns", ""),
					message->GetBool("all", false) ? 1 : 0))));
				fTarget.SendMessage(&kept);
				break;
			}
			case kChanges:
			{
				BMessage changed(kChanged);
				changed.AddMessage("counts", new BMessage(Take(rm_change_counts())));
				fTarget.SendMessage(&changed);
				break;
			}
			default:
				BLooper::MessageReceived(message);
		}
	}

private:
	static BString _Name(const BMessage& array, int32 i)
	{
		BString name;
		name << i;
		return array.GetString(name.String(), "");
	}

	BMessenger fTarget;
};


// #pragma mark - Library box


/*! A library in the sidebar: a small box with its name (a checkbox: shown
	on the map or not) and, under it, what's known of it (a map, how many
	records and peers). Right-click for what can be done with it. */
class LibraryItem : public BView {
public:
	LibraryItem(const BString& ns, const BString& detail, bool shown,
		BHandler* target)
		:
		BView(ns.String(), B_WILL_DRAW | B_FRAME_EVENTS),
		fNs(ns)
	{
		SetViewUIColor(B_PANEL_BACKGROUND_COLOR);
		BMessage* toggle = new BMessage(kLibraries);
		fCheck = new BCheckBox(ns.String(), ns.String(), toggle);
		fCheck->SetValue(shown ? B_CONTROL_ON : B_CONTROL_OFF);
		fCheck->SetTarget(target);
		fDetail = new BStringView(NULL, detail.String());
		BFont small(be_plain_font);
		small.SetSize(small.Size() * 0.85);
		fDetail->SetFont(&small);
		fDetail->SetHighColor(mix_color(ui_color(B_PANEL_TEXT_COLOR),
			ui_color(B_PANEL_BACKGROUND_COLOR), 150));
		fDetail->SetTruncation(B_TRUNCATE_END);
		float em = be_plain_font->Size();
		BLayoutBuilder::Group<>(this, B_VERTICAL, 0)
			.SetInsets(em * 0.4, em * 0.25, em * 0.4, em * 0.35)
			.Add(fCheck)
			.AddGroup(B_HORIZONTAL, 0)
				.SetInsets(em * 1.6, 0, 0, 0)
				.Add(fDetail)
			.End();
	}

	const BString& Ns() const { return fNs; }
	BCheckBox* Check() const { return fCheck; }

	void Draw(BRect)
	{
		BRect r = Bounds().InsetByCopy(0.5, 0.5);
		SetHighColor(tint_color(ui_color(B_PANEL_BACKGROUND_COLOR),
			B_DARKEN_2_TINT));
		StrokeRoundRect(r, 4, 4);
	}

	void FrameResized(float, float)
	{
		Invalidate();
	}

private:
	BString fNs;
	BCheckBox* fCheck;
	BStringView* fDetail;
};


/*! "95,335": a count, its thousands set apart. */
static BString
Grouped(int64 n)
{
	BString digits;
	digits << n;
	BString out;
	int32 length = digits.Length();
	for (int32 i = 0; i < length; i++) {
		if (i > 0 && (length - i) % 3 == 0)
			out << ",";
		out << digits.ByteAt(i);
	}
	return out;
}


// #pragma mark - Places


struct Place {
	int32 index;
	BString ns, id, kind, title, detail;
	bool hasThumb;
	double lat, lon;
	bool hasMinZoom;
	double minZoom;
};


static rgb_color
KindColor(const BString& kind)
{
	if (kind == "photo")
		return make_color(0x35, 0x84, 0xe4);
	if (kind == "person")
		return make_color(0x26, 0xa2, 0x69);
	if (kind == "event")
		return make_color(0xe6, 0x61, 0x00);
	return make_color(0x91, 0x41, 0xac);
}


static const char*
KindGlyph(const BString& kind)
{
	if (kind == "photo")
		return "▣";
	if (kind == "person")
		return "☺";
	if (kind == "event")
		return "▦";
	return "●";
}


// #pragma mark - Photo window


/*! A photo, large, in a window of its own. */
class PhotoView : public BView {
public:
	PhotoView(BBitmap* bitmap)
		:
		BView("photo", B_WILL_DRAW | B_FULL_UPDATE_ON_RESIZE),
		fBitmap(bitmap)
	{
		SetViewColor(30, 30, 30);
	}

	~PhotoView() { delete fBitmap; }

	void SetBitmap(BBitmap* bitmap)
	{
		delete fBitmap;
		fBitmap = bitmap;
		Invalidate();
	}

	void Draw(BRect)
	{
		if (fBitmap == NULL)
			return;
		BRect b = Bounds(), s = fBitmap->Bounds();
		float scale = std::min(b.Width() / s.Width(), b.Height() / s.Height());
		float w = s.Width() * scale, h = s.Height() * scale;
		BRect r(0, 0, w, h);
		r.OffsetTo((b.Width() - w) / 2, (b.Height() - h) / 2);
		DrawBitmap(fBitmap, s, r);
	}

private:
	BBitmap* fBitmap;
};


class PhotoWindow : public BWindow {
public:
	PhotoWindow(const BString& title, BBitmap* shown)
		:
		BWindow(BRect(80, 60, 1080, 780), title.String(), B_TITLED_WINDOW,
			B_AUTO_UPDATE_SIZE_LIMITS | B_CLOSE_ON_ESCAPE)
	{
		fView = new PhotoView(shown);
		BLayoutBuilder::Group<>(this, B_VERTICAL, 0).Add(fView);
		// The whole screen, its tab and border just outside.
		BRect screen = BScreen(this).Frame();
		BRect frame = Frame(), outer = DecoratorFrame();
		float left = frame.left - outer.left, top = frame.top - outer.top;
		float right = outer.right - frame.right, bottom = outer.bottom - frame.bottom;
		MoveTo(screen.left + left, screen.top + top);
		ResizeTo(screen.Width() - left - right, screen.Height() - top - bottom);
	}

	void MessageReceived(BMessage* message)
	{
		if (message->what == kPreviewDone) {
			BBitmap* bitmap = NULL;
			if (message->FindPointer("bitmap", (void**)&bitmap) == B_OK
				&& bitmap != NULL)
				fView->SetBitmap(bitmap);
			return;
		}
		BWindow::MessageReceived(message);
	}

private:
	PhotoView* fView;
};


// #pragma mark - Bubble


/*! A photo's preview in the bubble; a click opens it larger. */
class PreviewView : public BView {
public:
	PreviewView(float width)
		:
		BView("preview", B_WILL_DRAW),
		fBitmap(NULL)
	{
		SetViewColor(B_TRANSPARENT_COLOR);
		SetExplicitSize(BSize(width, roundf(width * 0.67f)));
	}

	~PreviewView() { delete fBitmap; }

	void SetBitmap(BBitmap* bitmap)
	{
		delete fBitmap;
		fBitmap = bitmap;
		Invalidate();
	}

	const BBitmap* Bitmap() const { return fBitmap; }

	void Draw(BRect)
	{
		BRect b = Bounds();
		SetHighUIColor(B_PANEL_BACKGROUND_COLOR, B_DARKEN_1_TINT);
		FillRect(b);
		if (fBitmap == NULL) {
			SetHighUIColor(B_PANEL_TEXT_COLOR);
			DrawString("Loading…", BPoint(8, b.Height() / 2));
			return;
		}
		BRect s = fBitmap->Bounds();
		BPoint at((b.Width() - s.Width()) / 2, (b.Height() - s.Height()) / 2);
		DrawBitmap(fBitmap, at);
	}

	void MouseDown(BPoint)
	{
		if (fBitmap != NULL)
			Window()->PostMessage(kOpenPhoto);
	}

private:
	BBitmap* fBitmap;
};


/*! The info bubble of a place clicked on the map, kept over its place as
	the map moves (the map draws its pointer). What it shows depends on the
	record: a photo's preview, a contact's details, an event's time and
	place, a place's category and hours. */
class Bubble : public BView {
public:
	Bubble(const Place& p, int32 here, BHandler* map)
		:
		BView("bubble", B_WILL_DRAW | B_FRAME_EVENTS),
		fPlace(p),
		fPreview(NULL),
		// As wide at any font size: its text, close button and preview fit.
		fWidth(roundf(be_plain_font->Size() * 24))
	{
		SetViewUIColor(B_PANEL_BACKGROUND_COLOR);
		BString title(p.title);
		if (here > 1)
			title << " (and " << (here - 1) << " more here)";
		BStringView* head = new BStringView(NULL, title.String());
		head->SetFont(be_bold_font);
		head->SetTruncation(B_TRUNCATE_END);
		// A long title shortens ("…"): the close button stays in the bubble.
		head->SetExplicitMinSize(BSize(60, B_SIZE_UNSET));
		BButton* close = new BButton("close", "×", new BMessage(kCloseBubble));
		close->SetTarget(map);
		close->SetFlat(true);
		close->SetExplicitSize(BSize(22, 22));
		BString what(p.detail.IsEmpty() ? _KindName(p.kind) : p.detail.String());
		what << "  ·  " << p.ns;
		fBody = new BGroupView(B_VERTICAL, 2);
		BLayoutBuilder::Group<>(this, B_VERTICAL, 4)
			.SetInsets(10, 6, 6, 10)
			.AddGroup(B_HORIZONTAL, 4)
				.Add(head)
				.AddGlue()
				.Add(close)
			.End()
			.Add(_Dim(what))
			.Add(fBody);
		if (p.kind == "photo") {
			fPreview = new PreviewView(fWidth - 16);
			fBody->AddChild(fPreview);
		}
	}

	const Place& Shown() const { return fPlace; }
	PreviewView* Preview() const { return fPreview; }

	void Draw(BRect)
	{
		SetHighColor(tint_color(ui_color(B_PANEL_BACKGROUND_COLOR),
			B_DARKEN_3_TINT));
		StrokeRect(Bounds());
	}

	/*! The record's details, by what it is. */
	void Fill(const BMessage& details)
	{
		std::vector<std::pair<BString, BString> > attrs;
		for (int32 i = 0; i < Count(details); i++) {
			BMessage pair = Item(details, i);
			attrs.push_back(std::make_pair(BString(pair.GetString("0", "")),
				BString(pair.GetString("1", ""))));
		}
		auto get = [&](const char* key) {
			for (auto& a : attrs) {
				if (a.first == key)
					return a.second;
			}
			return BString();
		};
		// "phone", "phone:work": labelled values.
		auto labelled = [&](const char* prefix, const char* plain) {
			BString colon(prefix);
			colon << ":";
			for (auto& a : attrs) {
				if (a.first == prefix)
					_Line(plain, a.second);
				else if (a.first.StartsWith(colon)) {
					BString label(a.first);
					label.Remove(0, colon.Length());
					_Line(label.String(), a.second);
				}
			}
		};
		const BString& kind = fPlace.kind;
		if (kind == "photo") {
			_Line("Taken", _When(get("taken")));
			BString camera(get("camera:make"));
			camera << " " << get("camera:model");
			_Line("Camera", camera.Trim());
		} else if (kind == "person") {
			labelled("phone", "Phone");
			labelled("email", "E-mail");
			labelled("address", "Address");
			_Line("Birthday", get("birthday"));
			_Line("Note", get("note"));
		} else if (kind == "event") {
			BString start = get("start"), end = get("end"), span;
			if (!start.IsEmpty() && !end.IsEmpty()) {
				BString endShown(end);
				if (start.Length() >= 10 && end.Length() >= 16
					&& BString(start.String(), 10) == BString(end.String(), 10))
					end.CopyInto(endShown, 11, 5);
				else
					endShown = _When(end);
				span << _When(start) << " – " << endShown;
			} else if (!start.IsEmpty())
				span = _When(start);
			_Line("When", span);
			_Line("Where", get("location"));
			_Line("Repeats", get("rrule"));
			BString people;
			for (auto& a : attrs) {
				if (a.first.StartsWith("attendee:")) {
					if (!people.IsEmpty())
						people << ", ";
					people << a.second;
				}
			}
			_Line("With", people);
			_Line("Note", get("note"));
		} else {
			_Line("Address", get("address"));
			_Line("Open", get("opening_hours"));
			_Line("Phone", get("phone"));
			_Line("Website", get("website"));
			_Line("Cuisine", get("cuisine"));
			_Line("Note", get("note"));
		}
		BString where;
		where.SetToFormat("%.5f, %.5f", fPlace.lat, fPlace.lon);
		fBody->AddChild(_Dim(where));
		Fit();
	}

	/*! Its width, and as tall as its contents now (its text shortened). */
	void Fit()
	{
		InvalidateLayout(true);
		ResizeTo(fWidth, GetLayout()->PreferredSize().height);
		Layout(true);
	}

private:
	static const char* _KindName(const BString& kind)
	{
		if (kind == "photo")
			return "Photo";
		if (kind == "person")
			return "Contact";
		if (kind == "event")
			return "Event";
		return "Place";
	}

	/*! "2026-10-25T17:00" as "25.10.2026 17:00". */
	static BString _When(const BString& t)
	{
		if (t.Length() < 10)
			return t;
		BString out;
		out << BString(t.String() + 8, 2) << "." << BString(t.String() + 5, 2)
			<< "." << BString(t.String(), 4);
		if (t.Length() >= 16)
			out << " " << BString(t.String() + 11, 5);
		return out;
	}

	static BStringView* _Dim(const BString& text)
	{
		BStringView* v = new BStringView(NULL, text.String());
		BFont small(be_plain_font);
		small.SetSize(small.Size() * 0.85);
		v->SetFont(&small);
		v->SetHighColor(mix_color(ui_color(B_PANEL_TEXT_COLOR),
			ui_color(B_PANEL_BACKGROUND_COLOR), 150));
		v->SetTruncation(B_TRUNCATE_END);
		v->SetExplicitMinSize(BSize(40, B_SIZE_UNSET));
		return v;
	}

	void _Line(const char* label, const BString& value)
	{
		BString v(value);
		if (v.Trim().IsEmpty())
			return;
		BStringView* l = _Dim(label);
		l->SetExplicitSize(BSize(70, B_SIZE_UNSET));
		BStringView* text = new BStringView(NULL, v.String());
		text->SetTruncation(B_TRUNCATE_END);
		text->SetExplicitMaxSize(BSize(B_SIZE_UNLIMITED, B_SIZE_UNSET));
		text->SetExplicitMinSize(BSize(40, B_SIZE_UNSET));
		BGroupView* row = new BGroupView(B_HORIZONTAL, 6);
		row->AddChild(l);
		row->AddChild(text);
		fBody->AddChild(row);
	}

	Place fPlace;
	BGroupView* fBody;
	PreviewView* fPreview;
	float fWidth;
};


// #pragma mark - The map


class MapView : public BView {
public:
	MapView()
		:
		BView("map", B_WILL_DRAW | B_FRAME_EVENTS | B_FULL_UPDATE_ON_RESIZE
			| B_NAVIGABLE | B_PULSE_NEEDED),
		fZoom(2),
		fCenterX(WorldX(25.0, 2)),
		fCenterY(WorldY(58.7, 2)),
		fBackground(make_color(0xf2, 0xef, 0xe9)),
		fDragging(false),
		fMoved(false),
		fNextWorker(0),
		fUse(0)
	{
		SetViewColor(B_TRANSPARENT_COLOR);
	}

	~MapView()
	{
		_Clear();
		for (auto& t : fThumbs)
			delete t.second;
		for (auto& i : fIcons)
			delete i.second.bitmap;
	}

	/*! Shows the bubble of the place with this number, if it's loaded. */
	void OpenBubble(int32 index)
	{
		for (const Place& p : fPlaces) {
			if (p.index == index) {
				_OpenBubble(p, 1);
				return;
			}
		}
	}

	/*! Puts lat, lon in the middle at `zoom`. */
	void CenterOn(double lat, double lon, int zoom)
	{
		_SetZoom(std::max(0, std::min(kMaxZoom, zoom)));
		fCenterX = WorldX(lon, fZoom);
		fCenterY = WorldY(lat, fZoom);
		Invalidate();
		_Changed();
	}

	/*! The photo of the open bubble, large, in a window of its own. */
	void OpenPhoto()
	{
		if (fBubble == NULL || fBubble->Preview() == NULL
			|| fBubble->Preview()->Bitmap() == NULL)
			return;
		PhotoWindow* w = new PhotoWindow(fBubble->Shown().title,
			new BBitmap(fBubble->Preview()->Bitmap()));
		w->Show();
		BMessage ask(kPreview);
		ask.AddInt32("i", fBubble->Shown().index);
		ask.AddMessenger("reply", BMessenger(w));
		if (!fWorkers.empty())
			fWorkers[0]->PostMessage(&ask);
	}

	/*! Tiles that failed are asked again: a redraw does it. */
	void Pulse()
	{
		if (!fFailed.empty())
			Invalidate();
	}

	void AttachedToWindow()
	{
		Window()->SetPulseRate(5000000);
		for (int i = 0; i < 3; i++) {
			TileWorker* w = new TileWorker(BMessenger(this));
			w->Run();
			fWorkers.push_back(w);
		}
	}

	void DetachedFromWindow()
	{
		for (TileWorker* w : fWorkers) {
			w->Lock();
			w->Quit();
		}
		fWorkers.clear();
	}

	/*! Shows map `id` (its style read: its background), or none (""). */
	void SetMap(const BString& id, rgb_color background, const BString& credit)
	{
		fMap = id;
		fBackground = background;
		fCredit = credit;
		_Clear();
		sGeneration++;
		Invalidate();
	}

	void SetPlaces(const std::vector<Place>& places)
	{
		fPlaces = places;
		Invalidate();
	}

	/*! The libraries whose places aren't shown. */
	void SetHidden(const std::set<BString>& libraries)
	{
		fHidden = libraries;
		Invalidate();
	}

	/*! Fits west, south, east, north in the view. */
	void Fit(double west, double south, double east, double north)
	{
		BRect b = Bounds();
		int z = kMaxZoom;
		for (; z > 0; z--) {
			if (WorldX(east, z) - WorldX(west, z) <= b.Width()
				&& WorldY(south, z) - WorldY(north, z) <= b.Height())
				break;
		}
		_SetZoom(std::min(z, 16));
		fCenterX = (WorldX(west, fZoom) + WorldX(east, fZoom)) / 2;
		fCenterY = (WorldY(north, fZoom) + WorldY(south, fZoom)) / 2;
		Invalidate();
	}

	void ZoomBy(int steps, BPoint around)
	{
		int z = std::max(0, std::min(kMaxZoom, fZoom + steps));
		if (z == fZoom)
			return;
		BRect b = Bounds();
		// The world point under `around` stays there.
		double wx = fCenterX + around.x - b.Width() / 2;
		double wy = fCenterY + around.y - b.Height() / 2;
		double f = pow(2.0, z - fZoom);
		fCenterX = wx * f - (around.x - b.Width() / 2);
		fCenterY = wy * f - (around.y - b.Height() / 2);
		_SetZoom(z);
		Invalidate();
		_Changed();
	}

	void Draw(BRect update)
	{
		BRect b = Bounds();
		double left = fCenterX - b.Width() / 2;
		double top = fCenterY - b.Height() / 2;
		int n = 1 << fZoom;
		int x0 = (int)floor(left / kTileSize), x1 = (int)floor((left + b.Width()) / kTileSize);
		int y0 = (int)floor(top / kTileSize), y1 = (int)floor((top + b.Height()) / kTileSize);
		fUse++;
		std::vector<std::pair<BPoint, TileImage*> > shown;
		for (int ty = y0; ty <= y1; ty++) {
			for (int tx = x0; tx <= x1; tx++) {
				BPoint at(tx * kTileSize - left, ty * kTileSize - top);
				BRect r(at, at + BPoint(kTileSize - 1, kTileSize - 1));
				if (!r.Intersects(update))
					continue;
				if (ty < 0 || ty >= n || fMap.IsEmpty()) {
					SetHighColor(fBackground);
					FillRect(r);
					continue;
				}
				int wx = ((tx % n) + n) % n;
				TileImage* tile = _Tile(fZoom, wx, ty);
				if (tile == NULL) {
					// Until it comes: the nearest coarser tile here, enlarged.
					if (!_DrawAncestor(fZoom, wx, ty, r)) {
						SetHighColor(fBackground);
						FillRect(r);
					}
					continue;
				}
				DrawBitmap(tile->bitmap, at);
				shown.push_back(std::make_pair(at, tile));
			}
		}
		_DrawLabels(shown);
		_DrawMarkers();
		_DrawPointer();
		_DrawControls();
		_DrawLoading();
	}

	void MouseDown(BPoint where)
	{
		MakeFocus(true);
		if (_ControlAt(where) != 0) {
			ZoomBy(_ControlAt(where), BPoint(Bounds().Width() / 2,
				Bounds().Height() / 2));
			return;
		}
		int32 clicks = 1;
		Window()->CurrentMessage()->FindInt32("clicks", &clicks);
		if (clicks == 2) {
			ZoomBy(1, where);
			return;
		}
		fDragging = true;
		fMoved = false;
		fLast = where;
		SetMouseEventMask(B_POINTER_EVENTS, B_LOCK_WINDOW_FOCUS);
	}

	void MouseMoved(BPoint where, uint32 transit, const BMessage* drag)
	{
		if (!fDragging)
			return;
		BPoint d = where - fLast;
		if (fabs(d.x) + fabs(d.y) > 0) {
			fCenterX -= d.x;
			fCenterY -= d.y;
			fLast = where;
			fMoved = true;
			Invalidate();
		}
	}

	void MouseUp(BPoint where)
	{
		bool clicked = fDragging && !fMoved;
		fDragging = false;
		if (fMoved)
			_Changed();
		if (!clicked)
			return;
		for (auto it = fHits.rbegin(); it != fHits.rend(); ++it) {
			BPoint d = where - it->center;
			if (d.x * d.x + d.y * d.y > it->radius * it->radius)
				continue;
			_Clicked(it->members);
			return;
		}
		// A click on the map itself closes the bubble.
		_CloseBubble();
	}

	void KeyDown(const char* bytes, int32 count)
	{
		BPoint middle(Bounds().Width() / 2, Bounds().Height() / 2);
		switch (bytes[0]) {
			case '+':
			case '=':
				ZoomBy(1, middle);
				break;
			case '-':
				ZoomBy(-1, middle);
				break;
			case B_ESCAPE:
				_CloseBubble();
				break;
			case B_LEFT_ARROW:
				fCenterX -= 64;
				Invalidate();
				break;
			case B_RIGHT_ARROW:
				fCenterX += 64;
				Invalidate();
				break;
			case B_UP_ARROW:
				fCenterY -= 64;
				Invalidate();
				break;
			case B_DOWN_ARROW:
				fCenterY += 64;
				Invalidate();
				break;
			default:
				BView::KeyDown(bytes, count);
		}
	}

	void MessageReceived(BMessage* message)
	{
		switch (message->what) {
			case B_MOUSE_WHEEL_CHANGED:
			{
				float dy = message->GetFloat("be:wheel_delta_y", 0);
				BPoint where;
				uint32 buttons;
				GetMouse(&where, &buttons, false);
				if (dy != 0)
					ZoomBy(dy < 0 ? 1 : -1, where);
				break;
			}
			case kTileDone:
			{
				int32 z = message->GetInt32("z", 0), x = message->GetInt32("x", 0),
					y = message->GetInt32("y", 0);
				uint64 key = TileKey(z, x, y);
				fPending.erase(key);
				if (message->GetBool("failed", false)) {
					fFailed[key] = system_time();
					Invalidate();
					break;
				}
				TileImage* tile = NULL;
				if (message->FindPointer("tile", (void**)&tile) != B_OK
					|| tile == NULL)
					break;
				if (message->GetInt32("gen", -1) != sGeneration.load()) {
					delete tile->bitmap;
					delete tile;
					break;
				}
				tile->used = fUse;
				fTiles[key] = *tile;
				delete tile;
				_Trim();
				Invalidate();
				break;
			}
			case kThumbDone:
			{
				BBitmap* bitmap = NULL;
				message->FindPointer("bitmap", (void**)&bitmap);
				int32 i = message->GetInt32("i", -1);
				fprintf(stderr, "thumb %d arrived: %p\n", (int)i, bitmap);
				if (bitmap != NULL) {
					delete fThumbs[i];
					fThumbs[i] = bitmap;
					Invalidate();
				}
				break;
			}
			case kPreviewDone:
			{
				BBitmap* bitmap = NULL;
				message->FindPointer("bitmap", (void**)&bitmap);
				if (fBubble != NULL && fBubble->Preview() != NULL && bitmap != NULL
					&& message->GetInt32("i", -1) == fBubble->Shown().index) {
					BRect area = fBubble->Preview()->Bounds();
					fBubble->Preview()->SetBitmap(Scaled(bitmap,
						area.IntegerWidth() + 1, area.IntegerHeight() + 1, false));
					fBubble->Fit();
					_PlaceBubble();
				}
				delete bitmap;
				break;
			}
			case kDetailsDone:
			{
				if (fBubble == NULL
					|| fBubble->Shown().id != message->GetString("id", ""))
					break;
				BMessage details;
				message->FindMessage("details", &details);
				fBubble->Fill(details);
				_PlaceBubble();
				Invalidate();
				break;
			}
			case kCloseBubble:
				_CloseBubble();
				break;
			case kZoomIn:
			case kZoomOut:
				ZoomBy(message->what == kZoomIn ? 1 : -1,
					BPoint(Bounds().Width() / 2, Bounds().Height() / 2));
				break;
			default:
				BView::MessageReceived(message);
		}
	}

	/*! The view's area: west, south, east, north. */
	void Area(double* w, double* s, double* e, double* n) const
	{
		BRect b = Bounds();
		*w = Lon(fCenterX - b.Width() / 2, fZoom);
		*e = Lon(fCenterX + b.Width() / 2, fZoom);
		*n = Lat(fCenterY - b.Height() / 2, fZoom);
		*s = Lat(fCenterY + b.Height() / 2, fZoom);
	}

	int Zoom() const { return fZoom; }

private:
	struct Hit {
		BPoint center;
		float radius;
		std::vector<const Place*> members;
	};

	/*! The window is told the view changed (a lazy library's places there
		are asked for). */
	void _Changed()
	{
		if (Window() != NULL)
			Window()->PostMessage(kViewChanged);
		_PlaceBubble();
	}

	/*! A marker clicked: places apart, zoomed in on; one spot, its bubble. */
	void _Clicked(const std::vector<const Place*>& members)
	{
		const Place* first = members[0];
		bool spread = false;
		double w = first->lon, e = w, s = first->lat, n = s;
		for (const Place* p : members) {
			if (fabs(p->lat - first->lat) > 1e-5 || fabs(p->lon - first->lon) > 1e-5)
				spread = true;
			w = std::min(w, p->lon);
			e = std::max(e, p->lon);
			s = std::min(s, p->lat);
			n = std::max(n, p->lat);
		}
		if (members.size() > 1 && spread && fZoom < kMaxZoom) {
			int before = fZoom;
			double padW = (e - w) * 0.2, padH = (n - s) * 0.2;
			Fit(w - padW, s - padH, e + padW, n + padH);
			if (fZoom <= before)
				ZoomBy(before + 1 - fZoom, BPoint(Bounds().Width() / 2,
					Bounds().Height() / 2));
			_Changed();
			return;
		}
		_OpenBubble(*first, members.size());
	}

	void _OpenBubble(const Place& p, int32 here)
	{
		_CloseBubble();
		fBubble = new Bubble(p, here, this);
		AddChild(fBubble);
		fBubble->Fit();
		_PlaceBubble();
		if (fWorkers.empty())
			return;
		BMessage details(kDetails);
		details.AddString("ns", p.ns);
		details.AddString("id", p.id);
		details.AddMessenger("reply", BMessenger(this));
		fWorkers[0]->PostMessage(&details);
		if (p.kind == "photo") {
			BMessage preview(kPreview);
			preview.AddInt32("i", p.index);
			preview.AddMessenger("reply", BMessenger(this));
			fWorkers[1 % fWorkers.size()]->PostMessage(&preview);
		}
		Invalidate();
	}

	void _CloseBubble()
	{
		if (fBubble == NULL)
			return;
		RemoveChild(fBubble);
		delete fBubble;
		fBubble = NULL;
		Invalidate();
	}

	/*! The bubble above its place, its pointer's tip on it, kept on the map. */
	void _PlaceBubble()
	{
		if (fBubble == NULL)
			return;
		BRect b = Bounds();
		BPoint at = _Screen(fBubble->Shown().lat, fBubble->Shown().lon);
		BRect f = fBubble->Bounds();
		float x = std::max(0.0f, std::min((float)(at.x - f.Width() / 2),
			b.Width() - f.Width()));
		float y = std::max(0.0f, (float)(at.y - f.Height() - 12));
		if (fBubble->Frame().LeftTop() != BPoint(x, y))
			fBubble->MoveTo(x, y);
		bool shown = b.Contains(at);
		if (shown && fBubble->IsHidden())
			fBubble->Show();
		else if (!shown && !fBubble->IsHidden())
			fBubble->Hide();
	}

	void _DrawPointer()
	{
		_PlaceBubble();
		if (fBubble == NULL || fBubble->IsHidden())
			return;
		BPoint at = _Screen(fBubble->Shown().lat, fBubble->Shown().lon);
		BRect f = fBubble->Frame();
		float x = std::max(f.left + 8, std::min(f.right - 8, at.x));
		SetHighUIColor(B_PANEL_BACKGROUND_COLOR);
		FillTriangle(BPoint(x - 8, f.bottom), BPoint(x + 8, f.bottom),
			BPoint(at.x, at.y - 4));
		SetHighColor(tint_color(ui_color(B_PANEL_BACKGROUND_COLOR),
			B_DARKEN_3_TINT));
		StrokeLine(BPoint(x - 8, f.bottom), BPoint(at.x, at.y - 4));
		StrokeLine(BPoint(x + 8, f.bottom), BPoint(at.x, at.y - 4));
	}

	BPoint _Screen(double lat, double lon) const
	{
		BRect b = Bounds();
		return BPoint(WorldX(lon, fZoom) - (fCenterX - b.Width() / 2),
			WorldY(lat, fZoom) - (fCenterY - b.Height() / 2));
	}

	void _SetZoom(int z)
	{
		if (z != fZoom) {
			fZoom = z;
			sGeneration++;
			fPending.clear();
		}
	}

	TileImage* _Tile(int z, int x, int y)
	{
		uint64 key = TileKey(z, x, y);
		auto it = fTiles.find(key);
		if (it != fTiles.end()) {
			it->second.used = fUse;
			return &it->second;
		}
		auto failed = fFailed.find(key);
		if (failed != fFailed.end()) {
			if (system_time() - failed->second < 10000000)
				return NULL;
			fFailed.erase(failed);
		}
		if (fPending.find(key) == fPending.end() && !fWorkers.empty()) {
			fPending.insert(key);
			BMessage request(kTile);
			request.AddString("map", fMap);
			request.AddInt32("z", z);
			request.AddInt32("x", x);
			request.AddInt32("y", y);
			request.AddInt32("gen", sGeneration.load());
			request.AddUInt32("background", (uint32)fBackground.alpha << 24
				| (uint32)fBackground.red << 16 | (uint32)fBackground.green << 8
				| fBackground.blue);
			fWorkers[fNextWorker++ % fWorkers.size()]->PostMessage(&request);
		}
		return NULL;
	}

	bool _DrawAncestor(int z, int x, int y, BRect r)
	{
		for (int up = 1; up <= 5 && z - up >= 0; up++) {
			auto it = fTiles.find(TileKey(z - up, x >> up, y >> up));
			if (it == fTiles.end())
				continue;
			it->second.used = fUse;
			// Whole pixels of the ancestor, and no bilinear filter: app_server
			// crashed scaling a few pixels up bilinearly (BitmapPainter's
			// BilinearDefault::DrawToClipRect).
			int part = kTileSize >> up;
			int sx = (x - ((x >> up) << up)) * part;
			int sy = (y - ((y >> up) << up)) * part;
			DrawBitmap(it->second.bitmap, BRect(sx, sy, sx + part - 1, sy + part - 1),
				r);
			return true;
		}
		return false;
	}

	/*! "Loading map…" while tiles are being fetched (from other nodes, as
		they're looked at: a while each). */
	void _DrawLoading()
	{
		if (fPending.empty())
			return;
		BString text("Loading map… ");
		text << (int32)fPending.size();
		BFont small(be_plain_font);
		small.SetSize(11);
		SetFont(&small);
		float w = small.StringWidth(text.String());
		BRect b = Bounds();
		BRect box(b.left + 10, b.bottom - 30, b.left + 22 + w, b.bottom - 10);
		SetDrawingMode(B_OP_ALPHA);
		SetBlendingMode(B_PIXEL_ALPHA, B_ALPHA_OVERLAY);
		SetHighColor(255, 255, 255, 220);
		FillRoundRect(box, 6, 6);
		SetHighColor(60, 60, 60);
		DrawString(text.String(), BPoint(box.left + 6, box.bottom - 6));
		SetDrawingMode(B_OP_COPY);
		SetFont(be_plain_font);
	}

	/*! Keeps the tiles last drawn, dropping the oldest beyond a few screens. */
	void _Trim()
	{
		const size_t keep = 160;
		while (fTiles.size() > keep) {
			auto oldest = fTiles.begin();
			for (auto it = fTiles.begin(); it != fTiles.end(); ++it) {
				if (it->second.used < oldest->second.used)
					oldest = it;
			}
			delete oldest->second.bitmap;
			fTiles.erase(oldest);
		}
	}

	void _Clear()
	{
		for (auto& t : fTiles)
			delete t.second.bitmap;
		fTiles.clear();
		fPending.clear();
	}

	void _DrawLabels(const std::vector<std::pair<BPoint, TileImage*> >& shown)
	{
		std::vector<BRect> taken;
		SetDrawingMode(B_OP_ALPHA);
		SetBlendingMode(B_CONSTANT_ALPHA, B_ALPHA_OVERLAY);
		for (auto& s : shown) {
			for (const Label& l : s.second->labels) {
				BFont font(be_plain_font);
				font.SetSize(l.size);
				float w = font.StringWidth(l.text.String());
				font_height fh;
				font.GetHeight(&fh);
				float h = fh.ascent + fh.descent;
				BPoint anchor = s.first + BPoint(l.x * kTileSize, l.y * kTileSize);
				double a = l.angle * M_PI / 180.0;
				BPoint dir(cos(a), sin(a));
				BPoint down(-sin(a), cos(a));
				// Its box, upright around the anchor (rotated: the box of that).
				float bw = fabs(dir.x) * w + fabs(down.x) * h;
				float bh = fabs(dir.y) * w + fabs(down.y) * h;
				BRect box(anchor.x - bw / 2 - 2, anchor.y - bh / 2 - 2,
					anchor.x + bw / 2 + 2, anchor.y + bh / 2 + 2);
				bool clash = false;
				for (const BRect& t : taken) {
					if (t.Intersects(box)) {
						clash = true;
						break;
					}
				}
				if (clash)
					continue;
				taken.push_back(box);
				font.SetRotation(-l.angle);
				SetFont(&font);
				BPoint start = anchor - BPoint(dir.x * w / 2, dir.y * w / 2)
					+ BPoint(down.x * (fh.ascent - h / 2), down.y * (fh.ascent - h / 2));
				if (l.haloWidth > 0) {
					SetHighColor(l.halo);
					float r = std::max(1.0f, l.haloWidth);
					for (int i = 0; i < 8; i++) {
						double t = i * M_PI / 4;
						DrawString(l.text.String(),
							start + BPoint(cos(t) * r, sin(t) * r));
					}
				}
				SetHighColor(l.color);
				DrawString(l.text.String(), start);
			}
		}
		SetDrawingMode(B_OP_COPY);
		SetFont(be_plain_font);
	}

	struct Group {
		BString kind;
		std::vector<const Place*> members;
		double x, y;
	};

	void _DrawMarkers()
	{
		BRect b = Bounds();
		double left = fCenterX - b.Width() / 2, top = fCenterY - b.Height() / 2;
		std::map<std::string, Group> groups;
		for (const Place& p : fPlaces) {
			if (fHidden.find(p.ns) != fHidden.end())
				continue;
			if (p.hasMinZoom && fZoom < p.minZoom)
				continue;
			double x = WorldX(p.lon, fZoom) - left, y = WorldY(p.lat, fZoom) - top;
			if (x < -kCell || y < -kCell || x > b.Width() + kCell
				|| y > b.Height() + kCell)
				continue;
			BString key;
			key << p.kind << ":" << (int64)floor(x / kCell) << ":"
				<< (int64)floor(y / kCell);
			Group& g = groups[key.String()];
			g.kind = p.kind;
			g.members.push_back(&p);
		}
		SetDrawingMode(B_OP_ALPHA);
		SetBlendingMode(B_PIXEL_ALPHA, B_ALPHA_OVERLAY);
		BFont font(be_bold_font);
		fHits.clear();
		// Each marker where its places are; markers that would overlap are
		// pushed apart a few times, so none hides another.
		std::vector<std::pair<Group*, BPoint> > at;
		for (auto& e : groups) {
			double sx = 0, sy = 0;
			for (const Place* p : e.second.members) {
				sx += WorldX(p->lon, fZoom) - left;
				sy += WorldY(p->lat, fZoom) - top;
			}
			size_t n = e.second.members.size();
			at.push_back(std::make_pair(&e.second, BPoint(sx / n, sy / n)));
		}
		const float apart = 48;
		for (int round = 0; round < 8; round++) {
			bool moved = false;
			for (size_t i = 0; i < at.size(); i++) {
				for (size_t j = i + 1; j < at.size(); j++) {
					BPoint d = at[j].second - at[i].second;
					float len = sqrtf(d.x * d.x + d.y * d.y);
					if (len >= apart)
						continue;
					if (len < 0.5) {
						double a = 2 * M_PI * (j % 6) / 6;
						d = BPoint(cos(a), sin(a));
						len = 1;
					}
					float push = (apart - len) / 2 / len;
					at[i].second -= BPoint(d.x * push, d.y * push);
					at[j].second += BPoint(d.x * push, d.y * push);
					moved = true;
				}
			}
			if (!moved)
				break;
		}
		for (auto& a : at) {
			Group& g = *a.first;
			BPoint c = a.second;
			float r = 14;
			// A photo: its thumbnail (asked for once), in a white frame.
			BBitmap* thumb = NULL;
			if (g.kind == "photo") {
				for (const Place* p : g.members) {
					if (!p->hasThumb)
						continue;
					auto it = fThumbs.find(p->index);
					if (it != fThumbs.end()) {
						thumb = it->second;
						break;
					}
					if (fThumbAsked.insert(p->index).second && !fWorkers.empty()) {
						BMessage ask(kThumb);
						ask.AddInt32("i", p->index);
						fWorkers[fThumbAsked.size() % fWorkers.size()]->PostMessage(&ask);
					}
					break;
				}
			}
			if (thumb != NULL) {
				r = 20;
				BRect frame(c.x - r, c.y - r, c.x + r, c.y + r);
				SetHighColor(0, 0, 0, 70);
				FillRoundRect(frame.OffsetByCopy(0, 2).InsetByCopy(-2, -2), 5, 5);
				SetHighColor(255, 255, 255);
				FillRoundRect(frame.InsetByCopy(-2, -2), 5, 5);
				SetDrawingMode(B_OP_COPY);
				DrawBitmap(thumb, thumb->Bounds(), frame.InsetByCopy(1, 1));
				SetDrawingMode(B_OP_ALPHA);
			} else {
				// A place with a category (a map's café): its icon, on its
				// group's colour; else the kind's colour and glyph.
				const Icon* icon = NULL;
				if (g.kind == "place" && !g.members[0]->detail.IsEmpty())
					icon = _Icon(g.members[0]->detail);
				SetHighColor(0, 0, 0, 60);
				FillEllipse(c + BPoint(0, 1.5), r + 1, r + 1);
				SetHighColor(255, 255, 255);
				FillEllipse(c, r + 1.5, r + 1.5);
				SetHighColor(icon != NULL ? icon->color : KindColor(g.kind));
				FillEllipse(c, r, r);
			}
			if (thumb == NULL && g.kind == "place" && !g.members[0]->detail.IsEmpty()
				&& _Icon(g.members[0]->detail) != NULL) {
				const Icon* icon = _Icon(g.members[0]->detail);
				BRect i = icon->bitmap->Bounds();
				DrawBitmap(icon->bitmap, c - BPoint(i.Width() / 2, i.Height() / 2));
			} else if (thumb == NULL) {
				font.SetSize(13);
				SetFont(&font);
				SetHighColor(255, 255, 255);
				const char* glyph = KindGlyph(g.kind);
				DrawString(glyph, c - BPoint(font.StringWidth(glyph) / 2, -5));
			}
			Hit hit;
			hit.center = c;
			hit.radius = r + 3;
			hit.members = g.members;
			fHits.push_back(hit);
			if (g.members.size() > 1) {
				BString count;
				size_t n = g.members.size();
				if (n > 9999)
					count << (int32)(n / 1000) << "k";
				else
					count << (int32)n;
				font.SetSize(10);
				SetFont(&font);
				float w = font.StringWidth(count.String()) + 8;
				BRect badge(c.x + r - w / 2, c.y - r - 4, c.x + r + w / 2,
					c.y - r + 10);
				SetHighColor(40, 40, 40, 220);
				FillRoundRect(badge, 7, 7);
				SetHighColor(255, 255, 255);
				DrawString(count.String(), BPoint(badge.left + 4, badge.bottom - 3));
			}
		}
		SetDrawingMode(B_OP_COPY);
		SetFont(be_plain_font);
	}

	/*! The zoom buttons (top right) and the map's credit (bottom right). */
	BRect _Button(int which) const
	{
		BRect b = Bounds();
		float top = 12 + (which < 0 ? 34 : 0);
		return BRect(b.right - 42, top, b.right - 12, top + 30);
	}

	int _ControlAt(BPoint where) const
	{
		if (_Button(1).Contains(where))
			return 1;
		if (_Button(-1).Contains(where))
			return -1;
		return 0;
	}

	void _DrawControls()
	{
		SetDrawingMode(B_OP_ALPHA);
		SetBlendingMode(B_PIXEL_ALPHA, B_ALPHA_OVERLAY);
		BFont font(be_bold_font);
		font.SetSize(16);
		SetFont(&font);
		for (int which : {1, -1}) {
			BRect r = _Button(which);
			SetHighColor(255, 255, 255, 235);
			FillRoundRect(r, 6, 6);
			SetHighColor(0, 0, 0, 60);
			StrokeRoundRect(r, 6, 6);
			SetHighColor(60, 60, 60);
			const char* t = which > 0 ? "+" : "−";
			DrawString(t, BPoint(r.left + (r.Width() - font.StringWidth(t)) / 2,
				r.bottom - 9));
		}
		if (!fCredit.IsEmpty()) {
			BFont small(be_plain_font);
			small.SetSize(10);
			SetFont(&small);
			BString credit("© ");
			credit << fCredit;
			if (fCredit.StartsWith("©"))
				credit = fCredit;
			float w = small.StringWidth(credit.String());
			BRect b = Bounds();
			BRect r(b.right - w - 10, b.bottom - 16, b.right, b.bottom);
			SetHighColor(255, 255, 255, 200);
			FillRect(r);
			SetHighColor(60, 60, 60);
			DrawString(credit.String(), BPoint(r.left + 5, r.bottom - 4));
		}
		SetDrawingMode(B_OP_COPY);
		SetFont(be_plain_font);
	}

	int fZoom;
	double fCenterX, fCenterY;
	BString fMap;
	BString fCredit;
	rgb_color fBackground;
	std::map<uint64, TileImage> fTiles;
	std::set<uint64> fPending;
	std::map<uint64, bigtime_t> fFailed;
	std::vector<TileWorker*> fWorkers;
	std::vector<Place> fPlaces;
	std::set<BString> fHidden;
	struct Icon {
		BBitmap* bitmap;
		rgb_color color;
	};

	/*! A category's icon (made once, kept). */
	const Icon* _Icon(const BString& category)
	{
		auto it = fIcons.find(category);
		if (it != fIcons.end())
			return it->second.bitmap != NULL ? &it->second : NULL;
		const int size = 18;
		uint32 rgb = 0x6b6b6b;
		uint8* pixels = rm_icon(category.String(), size, &rgb);
		Icon icon = { NULL, make_color(rgb >> 16, (rgb >> 8) & 0xff, rgb & 0xff) };
		if (pixels != NULL) {
			icon.bitmap = new BBitmap(BRect(0, 0, size - 1, size - 1), B_RGBA32);
			icon.bitmap->SetBits(pixels, size * size * 4, 0, B_RGBA32);
			rm_free_bytes(pixels, size * size * 4);
		}
		fIcons[category] = icon;
		return icon.bitmap != NULL ? &fIcons[category] : NULL;
	}

	std::map<BString, Icon> fIcons;
	std::map<int32, BBitmap*> fThumbs;
	std::set<int32> fThumbAsked;
	std::vector<Hit> fHits;
	Bubble* fBubble = NULL;
	bool fDragging, fMoved;
	BPoint fLast;
	size_t fNextWorker;
	uint64 fUse;
};


// #pragma mark - Announces


/*! "map · 38822 places · 120 photos": what of a library the map shows. */
static BString
Holds(const BMessage& has)
{
	BString out;
	for (int32 i = 0; i < Count(has); i++) {
		BMessage pair = Item(has, i);
		BString type = pair.GetString("0", "");
		int64 n = (int64)pair.GetDouble("1", 0);
		BString word;
		if (type == "application/x-vnd.retifs.tileset")
			word = "map";
		else if (type == "application/vnd.mapbox-vector-tile")
			word << n << " map tiles";
		else if (type == "application/x-vnd.retifs.place")
			word << n << " places";
		else if (type.StartsWith("image/"))
			word << n << " photos";
		else if (type == "application/x-person" || type == "contact")
			word << n << " contacts";
		else if (type == "text/calendar" || type == "event")
			word << n << " events";
		if (word.IsEmpty())
			continue;
		if (!out.IsEmpty())
			out << " · ";
		out << word;
	}
	return out;
}


static BString
Ago(double ms)
{
	double s = (real_time_clock_usecs() / 1000.0 - ms) / 1000.0;
	BString out;
	if (s < 60)
		out = "just now";
	else if (s < 3600)
		out << (int32)(s / 60) << " min ago";
	else
		out << (int32)(s / 3600) << " h ago";
	return out;
}


/*!	Announces, as in LXMF apps (and the Linux map): the public libraries
	heard on the network that the map can use, each with Join. Read again
	every 5 s while open: announces keep coming.
*/
class AnnouncesWindow : public BWindow {
public:
	AnnouncesWindow(BLooper* worker)
		:
		BWindow(BRect(200, 120, 820, 700), "Announces", B_TITLED_WINDOW,
			B_AUTO_UPDATE_SIZE_LIMITS | B_CLOSE_ON_ESCAPE),
		fWorker(worker)
	{
		BStringView* intro = new BStringView("intro", "Public libraries heard "
			"on the network. Joining fetches only what you look at.");
		fList = new BGroupView(B_VERTICAL, B_USE_DEFAULT_SPACING);
		fList->GroupLayout()->SetInsets(B_USE_DEFAULT_SPACING);
		fList->SetViewUIColor(B_DOCUMENT_BACKGROUND_COLOR);
		BScrollView* scroll = new BScrollView("scroll", fList, 0, false, true);
		scroll->SetExplicitMinSize(BSize(580, 420));
		fMore = new BStringView("more", "");
		fMore->SetHighUIColor(B_PANEL_TEXT_COLOR, B_LIGHTEN_1_TINT);
		BLayoutBuilder::Group<>(this, B_VERTICAL)
			.SetInsets(B_USE_WINDOW_SPACING)
			.Add(intro)
			.Add(scroll)
			.Add(fMore);
		_Ask();
		BMessage tick(kHeard);
		tick.AddMessenger("reply", BMessenger(this));
		fRunner = new BMessageRunner(BMessenger(fWorker), &tick, 5000000);
	}

	~AnnouncesWindow()
	{
		delete fRunner;
	}

	void MessageReceived(BMessage* message)
	{
		switch (message->what) {
			case kHeardDone:
			{
				BMessage heard;
				message->FindMessage("heard", &heard);
				_Fill(heard);
				break;
			}
			case kJoinHeard:
			{
				BButton* b = NULL;
				message->FindPointer("source", (void**)&b);
				if (b != NULL) {
					b->SetLabel("Joining…");
					b->SetEnabled(false);
				}
				BMessage join(kJoinHeard);
				join.AddString("address", message->GetString("address", ""));
				join.AddMessenger("reply", BMessenger(this));
				fWorker->PostMessage(&join);
				break;
			}
			case kJoined:
			{
				BMessage result;
				message->FindMessage("result", &result);
				if (result.HasString("error")) {
					fError = "Not joined: ";
					fError << result.GetString("error", "");
				} else
					fError = "";
				_Ask();
				break;
			}
			default:
				BWindow::MessageReceived(message);
		}
	}

private:
	void _Ask()
	{
		BMessage ask(kHeard);
		ask.AddMessenger("reply", BMessenger(this));
		fWorker->PostMessage(&ask);
	}

	void _Fill(const BMessage& heard)
	{
		while (fList->CountChildren() > 0) {
			BView* v = fList->ChildAt(0);
			fList->RemoveChild(v);
			delete v;
		}
		while (fList->GroupLayout()->CountItems() > 0)
			delete fList->GroupLayout()->RemoveItem((int32)0);
		BMessage usable;
		heard.FindMessage("usable", &usable);
		if (Count(usable) == 0) {
			fList->AddChild(new BStringView(NULL, "Nothing heard yet: public "
				"libraries announced on the network show up here."));
		}
		for (int32 i = 0; i < Count(usable); i++) {
			BMessage l = Item(usable, i);
			BString name = l.GetString("name", "");
			if (name.IsEmpty())
				name = "A public library";
			BStringView* title = new BStringView(NULL, name.String());
			title->SetFont(be_bold_font);
			BString about = l.GetString("about", ""), detail;
			BMessage has;
			l.FindMessage("has", &has);
			if (l.GetBool("readable", true)) {
				BString node = l.GetString("node", "");
				node.Truncate(8);
				detail << Holds(has) << " · from <" << node << "…>, "
					<< (int32)l.GetDouble("hops", 0) << " hop(s), "
					<< Ago(l.GetDouble("heard_ms", 0));
			} else {
				detail << "Needs a newer retifs (announce v"
					<< (int32)l.GetDouble("v", 0) << ", format "
					<< (int32)l.GetDouble("fmt", 0) << ")";
			}
			BStringView* aboutView = new BStringView(NULL, about.String());
			BStringView* detailView = new BStringView(NULL, detail.String());
			detailView->SetHighUIColor(B_PANEL_TEXT_COLOR, B_LIGHTEN_1_TINT);
			BView* action;
			if (l.HasString("here")) {
				BStringView* label = new BStringView(NULL, "Joined");
				BString here("Here as ");
				here << l.GetString("here", "");
				label->SetToolTip(here.String());
				label->SetHighUIColor(B_PANEL_TEXT_COLOR, B_LIGHTEN_1_TINT);
				action = label;
			} else if (l.GetBool("readable", true)) {
				BMessage* join = new BMessage(kJoinHeard);
				join->AddString("address", l.GetString("address", ""));
				BButton* button = new BButton("Join", join);
				join->AddPointer("source", button);
				action = button;
			} else
				action = new BStringView(NULL, "");
			BGroupView* row = new BGroupView(B_HORIZONTAL);
			BLayoutBuilder::Group<>(row)
				.AddGroup(B_VERTICAL, 2)
					.Add(title)
					.Add(aboutView)
					.Add(detailView)
				.End()
				.AddGlue()
				.Add(action);
			fList->AddChild(row);
			if (i + 1 < Count(usable))
				fList->AddChild(new BSeparatorView(B_HORIZONTAL));
		}
		fList->GroupLayout()->AddItem(BSpaceLayoutItem::CreateGlue());
		BString more = fError;
		int32 other = (int32)heard.GetDouble("other", 0);
		if (more.IsEmpty() && other > 0) {
			more << other << " more heard that the map doesn't show (other kinds "
				"of data; the retifs app lists them).";
		}
		fMore->SetText(more);
	}

	BLooper* fWorker;
	BGroupView* fList;
	BStringView* fMore;
	BString fError;
	BMessageRunner* fRunner;
};


// #pragma mark - The window


class MapWindow : public BWindow {
public:
	MapWindow()
		:
		BWindow(BRect(80, 60, 1180, 780), "Map", B_TITLED_WINDOW,
			B_AUTO_UPDATE_SIZE_LIMITS | B_QUIT_ON_WINDOW_CLOSE),
		fFitted(false)
	{
		fMap = new MapView();
		fSearch = new BTextControl("search", NULL, "", NULL);
		fSearch->TextView()->SetExplicitMinSize(BSize(300, B_SIZE_UNSET));
		fAdd = new BButton("add", "+", NULL);
		fAdd->SetToolTip("Add a place");
		AddCommonFilter(new BMessageFilter(B_MOUSE_DOWN, &_RightClick));
		fAnnounces = new BButton("announces", "Announces", new BMessage(kAnnounces));
		fAnnounces->SetToolTip("Public libraries heard on the network");
		fAnnounces->SetExplicitMaxSize(BSize(B_SIZE_UNLIMITED, B_SIZE_UNSET));
		fStatus = new BStringView("status", "");
		fStatus->SetHighUIColor(B_PANEL_TEXT_COLOR, B_LIGHTEN_1_TINT);
		BFont small(be_plain_font);
		small.SetSize(small.Size() * 0.9);
		fStatus->SetFont(&small);

		fLibraries = new BGroupView(B_VERTICAL, 2);
		fReading = new BStatusBar("reading", "Reading libraries…");
		fReading->SetBarHeight(8);

		BGroupView* side = new BGroupView(B_VERTICAL, B_USE_DEFAULT_SPACING);
		BLayoutBuilder::Group<>(side)
			.SetInsets(B_USE_WINDOW_SPACING)
			.Add(fAnnounces)
			.Add(_Heading("Libraries"))
			.Add(fLibraries)
			.Add(fReading)
			.AddGlue()
			.Add(fStatus);
		// Wide enough for its text at any font size; the map takes the rest.
		float em = be_plain_font->Size();
		side->SetExplicitMinSize(BSize(em * 18, B_SIZE_UNSET));
		side->SetExplicitMaxSize(BSize(em * 30, B_SIZE_UNLIMITED));

		BLayoutBuilder::Group<>(this, B_HORIZONTAL, 0)
			.Add(side)
			.Add(new BSeparatorView(B_VERTICAL))
			.AddGroup(B_VERTICAL, 0)
				.AddGroup(B_HORIZONTAL)
					.SetInsets(B_USE_SMALL_SPACING)
					.Add(fAdd)
					.AddGlue()
					.Add(fSearch)
					.AddGlue()
				.End()
				.Add(new BSeparatorView(B_HORIZONTAL))
				.Add(fMap)
			.End();
		fMap->SetExplicitMinSize(BSize(500, 400));
		fSearch->SetModificationMessage(NULL);
		fSearch->SetMessage(new BMessage(kSearch));
		fSearch->SetToolTip("An address or a place: Enter searches");

		fWorker = new Worker(BMessenger(this));
		fWorker->Run();
		// Styles, searches and places by area on a worker of their own: not
		// behind a slow library's reading.
		fQuick = new Worker(BMessenger(this));
		fQuick->Run();
		fWorker->PostMessage(kLoad);
		BMessage changes(kChanges);
		fRunner = new BMessageRunner(BMessenger(fWorker), &changes, 5000000);
	}

	~MapWindow()
	{
		delete fRunner;
		fWorker->Lock();
		fWorker->Quit();
		fQuick->Lock();
		fQuick->Quit();
	}

	/*! The zoom button puts the window over the whole screen (its tab and
		border just outside), and back where it was. */
	void Zoom(BPoint, float, float)
	{
		if (fZoomed) {
			MoveTo(fRestore.LeftTop());
			ResizeTo(fRestore.Width(), fRestore.Height());
			fZoomed = false;
			return;
		}
		fRestore = Frame();
		BRect screen = BScreen(this).Frame();
		BRect frame = Frame(), outer = DecoratorFrame();
		float left = frame.left - outer.left, top = frame.top - outer.top;
		float right = outer.right - frame.right, bottom = outer.bottom - frame.bottom;
		MoveTo(screen.left + left, screen.top + top);
		ResizeTo(screen.Width() - left - right, screen.Height() - top - bottom);
		fZoomed = true;
	}

	void MessageReceived(BMessage* message)
	{
		switch (message->what) {
			case kLoadStart:
			{
				BMessage list;
				message->FindMessage("list", &list);
				if (list.HasString("error")) {
					fStatus->SetText(list.GetString("error", ""));
					break;
				}
				fNext = BMessage();
				fNextPlaces.clear();
				fNextByCell.clear();
				fReading->Reset("Reading libraries…");
				fReading->SetMaxValue(std::max<int32>(1, Count(*_Find(list,
					"libraries"))));
				if (fReading->IsHidden())
					fReading->Show();
				break;
			}
			case kLoadStep:
			{
				BString text(message->GetString("ns", ""));
				BString count;
				count << (message->GetInt32("i", 0) + 1) << " of "
					<< message->GetInt32("total", 0);
				fReading->Update(fReading->CurrentValue() == 0 ? 0.0001f : 1.0f,
					text.String(), count.String());
				break;
			}
			case kLoadLibrary:
			{
				BMessage data;
				message->FindMessage("data", &data);
				_AddLibrary(data);
				break;
			}
			case kLoadDone:
				fReading->Update(fReading->MaxValue() - fReading->CurrentValue());
				if (!fReading->IsHidden())
					fReading->Hide();
				break;
			case kStyled:
			{
				BMessage style;
				message->FindMessage("style", &style);
				BString id = message->GetString("map", "");
				if (style.HasString("error")) {
					fStatus->SetText(style.GetString("error", ""));
					break;
				}
				BString credit;
				for (int32 i = 0; i < Count(fTilesets); i++) {
					BMessage t = Item(fTilesets, i);
					if (BString(t.GetString("id", "")) == id)
						credit = t.GetString("attribution", "");
				}
				fMap->SetMap(id, Color((uint32)style.GetDouble("background",
					0xfff2efe9)), credit);
				break;
			}
			case kPickMap:
				_Pick(message->GetString("map", ""), true);
				break;
			case kAnnounces:
				(new AnnouncesWindow(fWorker))->Show();
				break;
			case kReload:
				fWorker->PostMessage(kLoad);
				break;
			case kLibraries:
			{
				// A map turned on is the one drawn (the last one turned on wins).
				BCheckBox* source = NULL;
				if (message->FindPointer("source", (void**)&source) == B_OK
					&& source != NULL && source->Value() == B_CONTROL_ON) {
					fMapOrder.remove(BString(source->Name()));
					fMapOrder.push_back(BString(source->Name()));
				}
				fHidden.clear();
				for (int32 i = 0; i < fLibraries->CountChildren(); i++) {
					LibraryItem* item = dynamic_cast<LibraryItem*>(fLibraries->ChildAt(i));
					if (item != NULL && item->Check()->Value() != B_CONTROL_ON)
						fHidden.insert(item->Ns());
				}
				fMap->SetHidden(fHidden);
				_ShowFirstMap();
				break;
			}
			case kToggle:
			{
				LibraryItem* item = _Item(message->GetString("ns", ""));
				if (item == NULL)
					break;
				item->Check()->SetValue(item->Check()->Value() == B_CONTROL_ON
					? B_CONTROL_OFF : B_CONTROL_ON);
				item->Check()->Invoke();
				break;
			}
			case kLeaveAsk:
			{
				BString ns = message->GetString("ns", "");
				BString text;
				text << "Leave " << ns << "?\n\nIts records go from this device. "
					"Others who have it keep theirs; you can join it again.";
				BAlert* alert = new BAlert("Leave", text.String(), "Cancel",
					"Leave", NULL, B_WIDTH_AS_USUAL, B_WARNING_ALERT);
				alert->SetShortcut(0, B_ESCAPE);
				fLeaving = ns;
				alert->Go(new BInvoker(new BMessage(kLeave), this));
				break;
			}
			case kLeave:
			{
				// The alert's answer: 1 is "Leave".
				if (message->GetInt32("which", 0) != 1 || fLeaving.IsEmpty())
					break;
				BMessage leave(kLeave);
				leave.AddString("ns", fLeaving);
				fWorker->PostMessage(&leave);
				fStatus->SetText(BString("Leaving ") << fLeaving << "…");
				fLeaving = "";
				break;
			}
			case kSearch:
			{
				BString text(fSearch->Text());
				if (text.Trim().IsEmpty())
					break;
				BMessage find(kFind);
				find.AddString("text", text);
				fQuick->PostMessage(&find);
				fStatus->SetText(BString("Searching ") << text << "…");
				break;
			}
			case kFound:
				_ShowFound(*message);
				break;
			case kPick:
				fMap->CenterOn(message->GetDouble("lat", 0),
					message->GetDouble("lon", 0),
					(int)round(message->GetDouble("zoom", 15)));
				if (message->GetInt32("place", -1) >= 0)
					fMap->OpenBubble(message->GetInt32("place", -1));
				break;
			case kOpenPhoto:
				fMap->OpenPhoto();
				break;
			case kViewChanged:
				_AskPlacesIn();
				break;
			case kPlacesFound:
			{
				BMessage places;
				message->FindMessage("places", &places);
				if (places.HasString("error") || Count(places) == 0)
					break;
				for (int32 i = 0; i < Count(places); i++)
					fNextPlaces.push_back(_Place(Item(places, i)));
				fMap->SetPlaces(fNextPlaces);
				break;
			}
			case kKeep:
			{
				BString ns = message->GetString("ns", "");
				fWorker->PostMessage(message);
				fStatus->SetText(message->GetBool("all", false)
					? BString("Keeping ") << ns << " on this device…"
					: BString("Keeping only what's used of ") << ns << "…");
				break;
			}
			case kKept:
			{
				BMessage result;
				message->FindMessage("result", &result);
				if (result.HasString("error")) {
					fStatus->SetText(result.GetString("error", ""));
					break;
				}
				fWorker->PostMessage(kLoad);
				break;
			}
			case kLeft:
			{
				BMessage result;
				message->FindMessage("result", &result);
				if (result.HasString("error")) {
					fStatus->SetText(result.GetString("error", ""));
					break;
				}
				fHidden.erase(message->GetString("ns", ""));
				fWorker->PostMessage(kLoad);
				break;
			}
			case kChanged:
			{
				BMessage counts;
				message->FindMessage("counts", &counts);
				if (fCounts.IsEmpty())
					fCounts = counts;
				else if (!_Same(fCounts, counts)) {
					fCounts = counts;
					fWorker->PostMessage(kLoad);
				}
				break;
			}
			default:
				BWindow::MessageReceived(message);
		}
	}

	/*! A right-click on a library: what can be done with it. */
	bool LibraryMenu(BView* view, BPoint where)
	{
		LibraryItem* item = NULL;
		for (BView* v = view; v != NULL && item == NULL; v = v->Parent())
			item = dynamic_cast<LibraryItem*>(v);
		if (item == NULL)
			return false;
		BPopUpMenu* menu = new BPopUpMenu("library", false, false);
		BMessage* toggle = new BMessage(kToggle);
		toggle->AddString("ns", item->Ns());
		menu->AddItem(new BMenuItem(item->Check()->Value() == B_CONTROL_ON
			? "Hide on the map" : "Show on the map", toggle));
		bool kept = fInfo[item->Ns()].GetBool("kept", false);
		BMessage* keep = new BMessage(kKeep);
		keep->AddString("ns", item->Ns());
		keep->AddBool("all", !kept);
		BMenuItem* keepItem = new BMenuItem("Keep on this device", keep);
		keepItem->SetMarked(kept);
		menu->AddItem(keepItem);
		menu->AddSeparatorItem();
		BMessage* leave = new BMessage(kLeaveAsk);
		leave->AddString("ns", item->Ns());
		BMenuItem* leaveItem = new BMenuItem("Leave library…", leave);
		// Your own library ("me") is this device's, not one joined.
		leaveItem->SetEnabled(item->Ns() != "me");
		menu->AddItem(leaveItem);
		menu->SetTargetForItems(this);
		view->ConvertToScreen(&where);
		menu->SetAsyncAutoDestruct(true);
		menu->Go(where, true, false, true);
		return true;
	}

private:
	static filter_result _RightClick(BMessage* message, BHandler** target,
		BMessageFilter* filter)
	{
		int32 buttons = message->GetInt32("buttons", 0);
		if ((buttons & B_SECONDARY_MOUSE_BUTTON) == 0)
			return B_DISPATCH_MESSAGE;
		BView* view = dynamic_cast<BView*>(*target);
		MapWindow* window = dynamic_cast<MapWindow*>(filter->Looper());
		BPoint where;
		if (view == NULL || window == NULL
			|| message->FindPoint("be:view_where", &where) != B_OK)
			return B_DISPATCH_MESSAGE;
		return window->LibraryMenu(view, where)
			? B_SKIP_MESSAGE : B_DISPATCH_MESSAGE;
	}

	LibraryItem* _Item(const BString& ns)
	{
		for (int32 i = 0; i < fLibraries->CountChildren(); i++) {
			LibraryItem* item = dynamic_cast<LibraryItem*>(fLibraries->ChildAt(i));
			if (item != NULL && item->Ns() == ns)
				return item;
		}
		return NULL;
	}

	BStringView* _Heading(const char* text)
	{
		BStringView* v = new BStringView(NULL, text);
		v->SetFont(be_bold_font);
		return v;
	}

	static bool _Same(const BMessage& a, const BMessage& b)
	{
		char* name;
		type_code type;
		for (int32 i = 0; a.GetInfo(B_ANY_TYPE, i, &name, &type) == B_OK; i++) {
			if (a.GetDouble(name, -1) != b.GetDouble(name, -1))
				return false;
		}
		return a.CountNames(B_ANY_TYPE) == b.CountNames(B_ANY_TYPE);
	}

	/*! One library read: its maps and places join those shown, at once
		(the next load builds up in fNext while the old one stays shown,
		so an update doesn't empty the window). */
	void _AddLibrary(const BMessage& data)
	{
		BString ns = data.GetString("ns", "");
		// A gazetteer is only searched: not listed.
		if (data.GetBool("address", false))
			return;
		BMessage tilesets, places;
		data.FindMessage("tilesets", &tilesets);
		data.FindMessage("places", &places);
		for (int32 i = 0; i < Count(tilesets); i++) {
			BString name;
			name << Count(fNext);
			fNext.AddMessage(name.String(), new BMessage(Item(tilesets, i)));
		}
		for (int32 i = 0; i < Count(places); i++)
			fNextPlaces.push_back(_Place(Item(places, i)));
		if (data.GetBool("by_cell", false))
			fNextByCell.insert(ns);
		fInfo[ns] = data;
		fTilesets = fNext;
		fMap->SetPlaces(fNextPlaces);
		fMap->SetHidden(fHidden);
		_FillLibraries();
		BString status;
		status << fNextPlaces.size() << " place(s)";
		fStatus->SetText(status);
		_ShowFirstMap();
	}

	static Place _Place(const BMessage& m)
	{
		Place p;
		p.index = (int32)m.GetDouble("i", 0);
		p.ns = m.GetString("ns", "");
		p.id = m.GetString("id", "");
		p.kind = m.GetString("kind", "place");
		p.title = m.GetString("title", "");
		p.detail = m.GetString("detail", "");
		p.hasThumb = m.GetBool("thumb", false);
		p.lat = m.GetDouble("lat", 0);
		p.lon = m.GetDouble("lon", 0);
		p.hasMinZoom = m.HasDouble("minzoom");
		p.minZoom = m.GetDouble("minzoom", 0);
		return p;
	}

	/*! A lazy library's places (a map's cafés) are asked for by area, once
		zoomed in enough to show them (zoom 12 and closer). */
	void _AskPlacesIn()
	{
		int zoom = fMap->Zoom();
		if (zoom < 12)
			return;
		double w, s, e, n;
		fMap->Area(&w, &s, &e, &n);
		for (const BString& ns : fNextByCell) {
			if (fHidden.find(ns) != fHidden.end())
				continue;
			BMessage ask(kPlacesIn);
			ask.AddString("ns", ns);
			ask.AddDouble("w", w);
			ask.AddDouble("s", s);
			ask.AddDouble("e", e);
			ask.AddDouble("n", n);
			ask.AddInt32("zoom", zoom);
			fQuick->PostMessage(&ask);
		}
	}

	/*! What a search found, as a menu under the search field. */
	void _ShowFound(const BMessage& message)
	{
		BMessage found;
		message.FindMessage("found", &found);
		if (found.HasString("error")) {
			fStatus->SetText(found.GetString("error", ""));
			return;
		}
		int32 count = Count(found);
		BString status;
		status << count << " found";
		fStatus->SetText(count == 0 ? BString("Nothing found") : status);
		if (count == 0)
			return;
		BPopUpMenu* menu = new BPopUpMenu("found", false, false);
		for (int32 i = 0; i < count && i < 15; i++) {
			BMessage f = Item(found, i);
			BMessage* pick = new BMessage(kPick);
			pick->AddDouble("lat", f.GetDouble("lat", 0));
			pick->AddDouble("lon", f.GetDouble("lon", 0));
			pick->AddDouble("zoom", f.GetDouble("zoom", 15));
			pick->AddInt32("place", f.HasDouble("place")
				? (int32)f.GetDouble("place", -1) : -1);
			menu->AddItem(new BMenuItem(f.GetString("name", "?"), pick));
		}
		menu->SetTargetForItems(this);
		BPoint where = fSearch->Bounds().LeftBottom();
		fSearch->ConvertToScreen(&where);
		menu->SetAsyncAutoDestruct(true);
		menu->Go(where, true, false, true);
	}

	/*! A checkbox per library with something the map shows: a map (its
		tiles), places, places held lazily by cell (a map's cafés). None for
		a library with nothing to put on a map, or a gazetteer (only
		searched). */
	void _FillLibraries()
	{
		std::map<BString, int32> counts;
		for (const Place& p : fNextPlaces)
			counts[p.ns]++;
		std::set<BString> maps;
		for (int32 i = 0; i < Count(fTilesets); i++) {
			BString ns = Item(fTilesets, i).GetString("ns", "");
			maps.insert(ns);
			if (counts.find(ns) == counts.end())
				counts[ns] = 0;
		}
		for (const BString& ns : fNextByCell) {
			if (counts.find(ns) == counts.end())
				counts[ns] = -1;
		}
		while (fLibraries->CountChildren() > 0) {
			BView* v = fLibraries->ChildAt(0);
			fLibraries->RemoveChild(v);
			delete v;
		}
		for (auto& c : counts) {
			const BMessage& info = fInfo[c.first];
			BString detail;
			if (maps.find(c.first) != maps.end())
				detail << "map  ·  ";
			if (c.second > 0)
				detail << Grouped(c.second) << " on the map  ·  ";
			int64 records = (int64)info.GetDouble("records", 0);
			detail << Grouped(records) << (records == 1 ? " record" : " records");
			if (info.GetBool("lazy", false))
				detail << " here";
			if (info.GetBool("kept", false))
				detail << "  ·  on this device";
			int32 peers = (int32)info.GetDouble("peers", 0);
			detail << "  ·  " << peers << (peers == 1 ? " peer" : " peers");
			fLibraries->AddChild(new LibraryItem(c.first, detail,
				fHidden.find(c.first) == fHidden.end(), this));
		}
		if (counts.empty())
			fLibraries->AddChild(new BStringView(NULL, "None with places yet"));
	}

	BMessage* _Find(const BMessage& data, const char* name)
	{
		static BMessage found;
		found.MakeEmpty();
		data.FindMessage(name, &found);
		return &found;
	}

	/*! Draws the map of the first library checked that has one (none: a
		blank map). */
	void _ShowFirstMap()
	{
		// The libraries turned on last first, then the rest as listed.
		std::vector<BString> order(fMapOrder.rbegin(), fMapOrder.rend());
		for (int32 i = 0; i < Count(fTilesets); i++)
			order.push_back(Item(fTilesets, i).GetString("ns", ""));
		for (const BString& ns : order) {
			if (fHidden.find(ns) != fHidden.end())
				continue;
			for (int32 i = 0; i < Count(fTilesets); i++) {
				BMessage t = Item(fTilesets, i);
				if (t.GetBool("overlay", false) || ns != t.GetString("ns", ""))
					continue;
				BString id = t.GetString("id", "");
				if (id != fShownMap)
					_Pick(id, true);
				return;
			}
		}
		if (!fShownMap.IsEmpty() || fTilesets.IsEmpty())
			_Pick("", false);
	}

	/*! Shows map `id`; on the first, fits its area (as the Linux app opens). */
	void _Pick(const BString& id, bool fit)
	{
		fShownMap = id;
		if (id.IsEmpty()) {
			fMap->SetMap("", make_color(0xf2, 0xef, 0xe9), "");
			return;
		}
		for (int32 i = 0; i < Count(fTilesets); i++) {
			BMessage t = Item(fTilesets, i);
			if (BString(t.GetString("id", "")) != id)
				continue;
			BMessage bounds;
			if (fit && !fFitted && t.FindMessage("bounds", &bounds) == B_OK
				&& Count(bounds) == 4) {
				fMap->Fit(Number(bounds, 0), Number(bounds, 1), Number(bounds, 2),
					Number(bounds, 3));
				fFitted = true;
			}
		}
		BMessage style(kStyle);
		style.AddString("map", id);
		fQuick->PostMessage(&style);
	}

	MapView* fMap;
	BTextControl* fSearch;
	BButton* fAdd;
	BButton* fAnnounces;
	BStringView* fStatus;
	BGroupView* fLibraries;
	BStatusBar* fReading;
	BMessage fNext;
	std::vector<Place> fNextPlaces;
	std::set<BString> fNextByCell;
	std::map<BString, BMessage> fInfo;
	BString fLeaving;
	std::set<BString> fHidden;
	std::list<BString> fMapOrder;
	bool fZoomed = false;
	BRect fRestore;
	BMessage fTilesets;
	BMessage fCounts;
	BString fShownMap;
	bool fFitted;
	Worker* fWorker;
	Worker* fQuick;
	BMessageRunner* fRunner;
};


// #pragma mark - App


class MapApp : public BApplication {
public:
	MapApp() : BApplication(kSignature) {}

	void ReadyToRun()
	{
		(new MapWindow())->Show();
	}
};


int
main()
{
	MapApp app;
	app.Run();
	return 0;
}
