// Command bedrock-strict is a headless Bedrock test client that fails on anything the real client could
// reject: it joins a server with gophertunnel's codec (every packet decoded, no unread bytes, no unknown
// packets), checks the value ranges the real client enforces, stays in the world, walks, chats, opens and
// closes its inventory, and exits non-zero on the first problem.
//
//	go run . -network raknet -addr 127.0.0.1:19132 -duration 60s
//	go run . -network nethernet -addr http://127.0.0.1:19132
package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"log/slog"
	"os"
	"reflect"
	"sort"
	"sync"
	"time"

	"github.com/go-gl/mathgl/mgl32"
	"github.com/sandertv/gophertunnel/minecraft"
	"github.com/sandertv/gophertunnel/minecraft/protocol"
	"github.com/sandertv/gophertunnel/minecraft/protocol/login"
	"github.com/sandertv/gophertunnel/minecraft/protocol/packet"
)

// problems collects every decode error the codec logs and every range violation found.
type problems struct {
	mu   sync.Mutex
	list []string
}

func (p *problems) add(format string, args ...any) {
	msg := fmt.Sprintf(format, args...)
	p.mu.Lock()
	p.list = append(p.list, msg)
	p.mu.Unlock()
	fmt.Fprintln(os.Stderr, "PROBLEM:", msg)
}

func (p *problems) count() int {
	p.mu.Lock()
	defer p.mu.Unlock()
	return len(p.list)
}

// logHandler turns every warning or error gophertunnel logs (decode failures among them) into a problem.
type logHandler struct {
	p     *problems
	attrs []slog.Attr
}

func (h logHandler) Enabled(_ context.Context, l slog.Level) bool { return l >= slog.LevelWarn }
func (h logHandler) Handle(_ context.Context, r slog.Record) error {
	msg := r.Message
	r.Attrs(func(a slog.Attr) bool { msg += " " + a.String(); return true })
	h.p.add("codec: %s", msg)
	return nil
}
func (h logHandler) WithAttrs(a []slog.Attr) slog.Handler {
	return logHandler{p: h.p, attrs: append(h.attrs, a...)}
}
func (h logHandler) WithGroup(string) slog.Handler { return h }

var (
	itemDescriptorCount = reflect.TypeOf(protocol.ItemDescriptorCount{})
	itemStack           = reflect.TypeOf(protocol.ItemStack{})
)

// walk checks the values inside a decoded packet that the real client range-checks.
func walk(p *problems, name string, v reflect.Value, path string) {
	switch v.Kind() {
	case reflect.Pointer, reflect.Interface:
		if !v.IsNil() {
			walk(p, name, v.Elem(), path)
		}
		return
	case reflect.Slice, reflect.Array:
		if v.Type().Elem().Kind() == reflect.Uint8 {
			return
		}
		for i := 0; i < v.Len(); i++ {
			walk(p, name, v.Index(i), fmt.Sprintf("%s[%d]", path, i))
		}
		return
	case reflect.Map:
		iter := v.MapRange()
		for iter.Next() {
			walk(p, name, iter.Value(), fmt.Sprintf("%s[%v]", path, iter.Key()))
		}
		return
	case reflect.Struct:
	default:
		return
	}
	switch v.Type() {
	case itemDescriptorCount:
		d := v.Interface().(protocol.ItemDescriptorCount)
		if d.Count < 0 || d.Count > 64 {
			p.add("%s%s: item descriptor count %d outside [0, 64]", name, path, d.Count)
		}
		if d.Descriptor == nil {
			p.add("%s%s: item descriptor missing", name, path)
		}
	case itemStack:
		s := v.Interface().(protocol.ItemStack)
		if s.NetworkID != 0 && s.Count == 0 {
			p.add("%s%s: item %d with count 0", name, path, s.NetworkID)
		}
		if s.NetworkID == 0 && s.Count != 0 {
			p.add("%s%s: air with count %d", name, path, s.Count)
		}
	}
	for i := 0; i < v.NumField(); i++ {
		if v.Type().Field(i).IsExported() {
			walk(p, name, v.Field(i), path+"."+v.Type().Field(i).Name)
		}
	}
}

func inRange[T ~int32 | ~int64 | ~uint8 | ~uint32 | ~int](p *problems, what string, v, lo, hi T) {
	if v < lo || v > hi {
		p.add("%s = %d outside [%d, %d]", what, v, lo, hi)
	}
}

// check validates one decoded packet beyond what the codec itself rejects.
func check(p *problems, pk packet.Packet) {
	name := fmt.Sprintf("%T", pk)
	walk(p, name, reflect.ValueOf(pk), "")
	switch pk := pk.(type) {
	case *packet.PlayerList:
		for i, e := range pk.Entries {
			if e.ActionType == protocol.PlayerListActionAdd {
				inRange(p, fmt.Sprintf("PlayerList.Entries[%d].BuildPlatform", i), e.BuildPlatform,
					int32(protocol.DeviceUnknown), int32(protocol.DeviceLinux))
			}
		}
	case *packet.AddPlayer:
		inRange(p, "AddPlayer.BuildPlatform", pk.BuildPlatform, int32(protocol.DeviceUnknown), int32(protocol.DeviceLinux))
	case *packet.Text:
		inRange(p, "Text.TextType", pk.TextType, packet.TextTypeRaw, packet.TextTypeObjectAnnouncement)
	case *packet.ContainerOpen:
		inRange(p, "ContainerOpen.ContainerType", int32(int8(pk.ContainerType)), protocol.ContainerTypeInventory, protocol.ContainerTypeCrafter)
	case *packet.ContainerClose:
		inRange(p, "ContainerClose.ContainerType", int32(int8(pk.ContainerType)), protocol.ContainerTypeInventory, protocol.ContainerTypeCrafter)
	case *packet.Disconnect:
		p.add("disconnected by the server: %q (reason %d)", pk.Message, pk.Reason)
	}
}

func main() {
	network := flag.String("network", "raknet", "transport: raknet or nethernet")
	addr := flag.String("addr", "127.0.0.1:19132", "server address (nethernet: http://host:port)")
	duration := flag.Duration("duration", 60*time.Second, "time to stay in the world after spawning")
	name := flag.String("name", "StrictBot", "player name")
	flag.Parse()

	p := &problems{}
	log := slog.New(logHandler{p: p})
	start := time.Now()
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	conn, err := minecraft.Dialer{
		ErrorLog:                   log,
		IdentityData:               login.IdentityData{DisplayName: *name},
		DisconnectOnInvalidPackets: true,
		DisconnectOnUnknownPackets: true,
	}.DialContext(ctx, *network, *addr)
	if err != nil {
		fmt.Println("FAIL: dial:", err)
		os.Exit(1)
	}
	defer conn.Close()
	fmt.Printf("logged in after %v\n", time.Since(start).Round(time.Millisecond))
	if err := conn.DoSpawnContext(ctx); err != nil {
		fmt.Println("FAIL: spawn:", err)
		os.Exit(1)
	}
	data := conn.GameData()
	fmt.Printf("spawned after %v at %v\n", time.Since(start).Round(time.Millisecond), data.PlayerPosition)

	var (
		mu         sync.Mutex
		pos        = data.PlayerPosition
		counts     = map[string]int{}
		openWindow = -1
		opened     bool
		closed     bool
		readErr    error
	)
	done := make(chan struct{})
	go func() {
		defer close(done)
		for {
			pk, err := conn.ReadPacket()
			if err != nil {
				mu.Lock()
				readErr = err
				mu.Unlock()
				return
			}
			check(p, pk)
			mu.Lock()
			counts[fmt.Sprintf("%T", pk)]++
			switch pk := pk.(type) {
			case *packet.MovePlayer:
				if pk.EntityRuntimeID == data.EntityRuntimeID {
					pos = pk.Position
				}
			case *packet.ContainerOpen:
				openWindow, opened = int(pk.WindowID), true
			case *packet.ContainerClose:
				closed = true
			}
			mu.Unlock()
		}
	}()

	ticker := time.NewTicker(50 * time.Millisecond)
	defer ticker.Stop()
	end := time.Now().Add(*duration)
	var tick uint64
	step := mgl32.Vec3{0.1, 0, 0}
	inventoryAt, closeAt, chatAt := 3*time.Second, 5*time.Second, 7*time.Second
	spawned := time.Now()
loop:
	for time.Now().Before(end) {
		select {
		case <-done:
			break loop
		case <-ticker.C:
		}
		tick++
		mu.Lock()
		if tick%100 == 0 {
			step = step.Mul(-1)
		}
		pos = pos.Add(step)
		cur := pos
		window := openWindow
		mu.Unlock()
		flags := protocol.NewInputFlags(packet.InputFlagCount)
		flags.Set(packet.InputFlagUp)
		_ = conn.WritePacket(&packet.PlayerAuthInput{
			Position:         cur,
			MoveVector:       mgl32.Vec2{0, 1},
			InputData:        flags,
			InputMode:        packet.InputModeMouse,
			PlayMode:         packet.PlayModeNormal,
			InteractionModel: packet.InteractionModelCrosshair,
			Tick:             tick,
			Delta:            step,
		})
		since := time.Since(spawned)
		switch {
		case since >= inventoryAt && since < inventoryAt+50*time.Millisecond:
			_ = conn.WritePacket(&packet.Interact{ActionType: packet.InteractActionOpenInventory, TargetEntityRuntimeID: data.EntityRuntimeID})
		case since >= closeAt && since < closeAt+50*time.Millisecond && window >= 0:
			_ = conn.WritePacket(&packet.ContainerClose{WindowID: byte(window)})
		case since >= chatAt && since < chatAt+50*time.Millisecond:
			_ = conn.WritePacket(&packet.Text{TextType: packet.TextTypeChat, SourceName: *name, Message: "strict client check", XUID: ""})
		}
	}
	_ = conn.Close()
	<-done

	mu.Lock()
	defer mu.Unlock()
	names := make([]string, 0, len(counts))
	total := 0
	for n, c := range counts {
		names = append(names, n)
		total += c
	}
	sort.Strings(names)
	for _, n := range names {
		fmt.Printf("  %-40s %d\n", n, counts[n])
	}
	fmt.Printf("received %d packets after spawn in %v\n", total, time.Since(spawned).Round(time.Millisecond))
	if !opened {
		p.add("no ContainerOpen after opening the inventory")
	} else if !closed {
		p.add("no ContainerClose after closing the inventory")
	}
	if readErr != nil && time.Now().Before(end) && !errors.Is(readErr, context.Canceled) {
		p.add("connection ended early: %v", readErr)
	}
	if n := p.count(); n > 0 {
		fmt.Printf("FAIL: %d problems\n", n)
		os.Exit(1)
	}
	fmt.Println("PASS")
}
