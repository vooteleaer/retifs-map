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
				BMessage loaded(kLoaded);
				loaded.AddMessage("data", new BMessage(Take(rm_load())));
				fTarget.SendMessage(&loaded);
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
	BMessenger fTarget;
};


// #pragma mark - Places


struct Place {
	int32 index;
	BString ns, id, kind, title;
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
		fDragging = false;
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
		for (auto& e : groups) {
			Group& g = e.second;
			double sx = 0, sy = 0;
			for (const Place* p : g.members) {
				sx += WorldX(p->lon, fZoom) - left;
				sy += WorldY(p->lat, fZoom) - top;
			}
			BPoint c(sx / g.members.size(), sy / g.members.size());
			const float r = 14;
			SetHighColor(0, 0, 0, 60);
			FillEllipse(c + BPoint(0, 1.5), r + 1, r + 1);
			SetHighColor(255, 255, 255);
			FillEllipse(c, r + 1.5, r + 1.5);
			SetHighColor(KindColor(g.kind));
			FillEllipse(c, r, r);
			font.SetSize(13);
			SetFont(&font);
			SetHighColor(255, 255, 255);
			const char* glyph = KindGlyph(g.kind);
			DrawString(glyph, c - BPoint(font.StringWidth(glyph) / 2, -5));
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
		fAnnounces = new BButton("announces", "Announces", new BMessage(kAnnounces));
		fAnnounces->SetToolTip("Public libraries heard on the network");
		fAnnounces->SetExplicitMaxSize(BSize(B_SIZE_UNLIMITED, B_SIZE_UNSET));
		fStatus = new BStringView("status", "");
		fStatus->SetHighUIColor(B_PANEL_TEXT_COLOR, B_LIGHTEN_1_TINT);
		BFont small(be_plain_font);
		small.SetSize(small.Size() * 0.9);
		fStatus->SetFont(&small);

		fLibraries = new BGroupView(B_VERTICAL, 2);

		BGroupView* side = new BGroupView(B_VERTICAL, B_USE_DEFAULT_SPACING);
		BLayoutBuilder::Group<>(side)
			.SetInsets(B_USE_WINDOW_SPACING)
			.Add(fAnnounces)
			.Add(_Heading("Libraries"))
			.Add(fLibraries)
			.AddGlue()
			.Add(fStatus);
		side->SetExplicitMinSize(BSize(240, B_SIZE_UNSET));
		side->SetExplicitMaxSize(BSize(320, B_SIZE_UNLIMITED));

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

		fWorker = new Worker(BMessenger(this));
		fWorker->Run();
		fWorker->PostMessage(kLoad);
		BMessage changes(kChanges);
		fRunner = new BMessageRunner(BMessenger(fWorker), &changes, 5000000);
	}

	~MapWindow()
	{
		delete fRunner;
		fWorker->Lock();
		fWorker->Quit();
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
			case kLoaded:
			{
				BMessage data;
				message->FindMessage("data", &data);
				_Loaded(data);
				break;
			}
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
					BCheckBox* box = dynamic_cast<BCheckBox*>(fLibraries->ChildAt(i));
					if (box != NULL && box->Value() != B_CONTROL_ON)
						fHidden.insert(box->Name());
				}
				fMap->SetHidden(fHidden);
				_ShowFirstMap();
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

private:
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

	void _Loaded(const BMessage& data)
	{
		if (data.HasString("error")) {
			fStatus->SetText(data.GetString("error", ""));
			return;
		}
		data.FindMessage("tilesets", &fTilesets);
		BMessage places, unplaced;
		data.FindMessage("places", &places);
		data.FindMessage("unplaced", &unplaced);

		std::vector<Place> list;
		for (int32 i = 0; i < Count(places); i++) {
			BMessage m = Item(places, i);
			Place p;
			p.index = (int32)m.GetDouble("i", i);
			p.ns = m.GetString("ns", "");
			p.id = m.GetString("id", "");
			p.kind = m.GetString("kind", "place");
			p.title = m.GetString("title", "");
			p.lat = m.GetDouble("lat", 0);
			p.lon = m.GetDouble("lon", 0);
			p.hasMinZoom = m.HasDouble("minzoom");
			p.minZoom = m.GetDouble("minzoom", 0);
			list.push_back(p);
		}
		fMap->SetPlaces(list);
		fMap->SetHidden(fHidden);
		_FillLibraries(data, list);

		BString status;
		status << list.size() << " place(s) in "
			<< Count(*_Find(data, "libraries")) << " libraries";
		fStatus->SetText(status);

		_ShowFirstMap();
	}

	/*! A checkbox per library with something the map shows: a map (its
		tiles), places, places held lazily (a map's cafés). None for a library
		with nothing to put on a map, or a gazetteer (only searched). */
	void _FillLibraries(const BMessage& data, const std::vector<Place>& places)
	{
		std::map<BString, int32> counts;
		for (const Place& p : places)
			counts[p.ns]++;
		BMessage lazy, address;
		// Held lazily, a library's places are known only by its index.
		data.FindMessage("lazy_places", &lazy);
		data.FindMessage("address", &address);
		std::set<BString> maps, skip;
		for (int32 i = 0; i < Count(fTilesets); i++) {
			BString ns = Item(fTilesets, i).GetString("ns", "");
			maps.insert(ns);
			if (counts.find(ns) == counts.end())
				counts[ns] = 0;
		}
		for (int32 i = 0; i < Count(address); i++) {
			BString name;
			name << i;
			skip.insert(address.GetString(name.String(), ""));
		}
		for (int32 i = 0; i < Count(lazy); i++) {
			BString name;
			name << i;
			BString ns = lazy.GetString(name.String(), "");
			if (skip.find(ns) == skip.end() && counts.find(ns) == counts.end())
				counts[ns] = -1;
		}
		while (fLibraries->CountChildren() > 0) {
			BView* v = fLibraries->ChildAt(0);
			fLibraries->RemoveChild(v);
			delete v;
		}
		for (auto& c : counts) {
			BString label(c.first);
			if (maps.find(c.first) != maps.end())
				label << "  ·  map";
			if (c.second > 0)
				label << "  ·  " << c.second;
			BCheckBox* box = new BCheckBox(c.first.String(), label.String(),
				new BMessage(kLibraries));
			box->SetValue(fHidden.find(c.first) == fHidden.end()
				? B_CONTROL_ON : B_CONTROL_OFF);
			box->SetTarget(this);
			fLibraries->AddChild(box);
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
		fWorker->PostMessage(&style);
	}

	MapView* fMap;
	BTextControl* fSearch;
	BButton* fAdd;
	BButton* fAnnounces;
	BStringView* fStatus;
	BGroupView* fLibraries;
	std::set<BString> fHidden;
	std::list<BString> fMapOrder;
	bool fZoomed = false;
	BRect fRestore;
	BMessage fTilesets;
	BMessage fCounts;
	BString fShownMap;
	bool fFitted;
	Worker* fWorker;
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
