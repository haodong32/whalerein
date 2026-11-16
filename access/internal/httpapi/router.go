package httpapi

import (
	"context"
	"net/http"
	"strings"
)

type routeSegment struct {
	literal string
	param   string
}

type routeSpec struct {
	method  string
	pattern string
	parts   []routeSegment
	handler func(http.ResponseWriter, *http.Request, map[string]string)
}

type pathMatch struct {
	route  *routeSpec
	params map[string]string
}

type router struct {
	routes      []routeSpec
	notFound    func(http.ResponseWriter, *http.Request)
	methodError func(http.ResponseWriter, *http.Request)
}

// allowedOrder is the deterministic order of the Allow header.
var allowedOrder = []string{
	http.MethodOptions, http.MethodGet, http.MethodHead,
	http.MethodPost, http.MethodPut, http.MethodPatch, http.MethodDelete,
}

func newRouter(routes []routeSpec) *router {
	r := &router{routes: make([]routeSpec, 0, len(routes))}
	for _, spec := range routes {
		spec.parts = compilePattern(spec.pattern)
		r.routes = append(r.routes, spec)
	}
	return r
}

func compilePattern(pattern string) []routeSegment {
	if pattern == "/" {
		return nil
	}
	raw := strings.Split(strings.TrimPrefix(pattern, "/"), "/")
	parts := make([]routeSegment, 0, len(raw))
	for _, segment := range raw {
		if strings.HasPrefix(segment, "{") && strings.HasSuffix(segment, "}") {
			parts = append(parts, routeSegment{param: strings.TrimSuffix(strings.TrimPrefix(segment, "{"), "}")})
		} else {
			parts = append(parts, routeSegment{literal: segment})
		}
	}
	return parts
}

// splitPath returns the path segments, or reports that the path cannot match
// any compiled route (missing leading slash, trailing slash, or empty).
func splitPath(path string) ([]string, bool) {
	if path == "/" {
		return nil, true
	}
	if !strings.HasPrefix(path, "/") || strings.HasSuffix(path, "/") {
		return nil, false
	}
	segments := strings.Split(strings.TrimPrefix(path, "/"), "/")
	for _, segment := range segments {
		if segment == "" {
			return nil, false
		}
	}
	return segments, true
}

func matchRoute(spec *routeSpec, segments []string) (map[string]string, bool) {
	if len(spec.parts) != len(segments) {
		return nil, false
	}
	if len(spec.parts) == 0 {
		return map[string]string{}, true
	}
	params := make(map[string]string)
	for index, part := range spec.parts {
		value := segments[index]
		if value == "" {
			return nil, false
		}
		if part.param != "" {
			params[part.param] = value
			continue
		}
		if part.literal != value {
			return nil, false
		}
	}
	return params, true
}

// match returns the first path match even when the method differs, so the
// caller can distinguish "wrong method" (405) from "unknown path" (404).
func (r *router) match(method, path string) (pathMatch, bool, bool) {
	segments, ok := splitPath(path)
	if !ok {
		return pathMatch{}, false, false
	}
	var first pathMatch
	pathMatched := false
	for i := range r.routes {
		spec := &r.routes[i]
		params, matched := matchRoute(spec, segments)
		if !matched {
			continue
		}
		if !pathMatched {
			first = pathMatch{route: spec, params: params}
			pathMatched = true
		}
		if spec.method == method {
			return pathMatch{route: spec, params: params}, true, true
		}
	}
	return first, pathMatched, false
}

// allowedMethods lists every method the path serves. GET implies HEAD, and
// OPTIONS is always advertised because the router answers it.
func (r *router) allowedMethods(path string) []string {
	segments, ok := splitPath(path)
	if !ok {
		return nil
	}
	present := make(map[string]bool)
	for i := range r.routes {
		if _, matched := matchRoute(&r.routes[i], segments); matched {
			present[r.routes[i].method] = true
		}
	}
	if len(present) == 0 {
		return nil
	}
	if present[http.MethodGet] {
		present[http.MethodHead] = true
	}
	present[http.MethodOptions] = true
	allowed := make([]string, 0, len(present))
	for _, method := range allowedOrder {
		if present[method] {
			allowed = append(allowed, method)
		}
	}
	return allowed
}

func (r *router) getRoute(path string) (*routeSpec, map[string]string, bool) {
	segments, ok := splitPath(path)
	if !ok {
		return nil, nil, false
	}
	for i := range r.routes {
		if r.routes[i].method != http.MethodGet {
			continue
		}
		if params, matched := matchRoute(&r.routes[i], segments); matched {
			return &r.routes[i], params, true
		}
	}
	return nil, nil, false
}

func (r *router) ServeHTTP(w http.ResponseWriter, req *http.Request) {
	match, pathMatched, methodMatched := r.match(req.Method, req.URL.Path)
	if !methodMatched && pathMatched {
		if allowed := r.allowedMethods(req.URL.Path); len(allowed) > 0 {
			w.Header().Set("Allow", strings.Join(allowed, ", "))
		}
		switch req.Method {
		case http.MethodOptions:
			// A probe can discover the method matrix without authenticating or
			// touching a handler.
			w.WriteHeader(http.StatusNoContent)
			return
		case http.MethodHead:
			// HEAD is served by the GET route; the response body is discarded
			// below while status and headers are preserved.
			if route, params, ok := r.getRoute(req.URL.Path); ok {
				match = pathMatch{route: route, params: params}
				methodMatched = true
			}
		}
	}
	if !methodMatched {
		if pathMatched && r.methodError != nil {
			r.methodError(w, req)
			return
		}
		if r.notFound != nil {
			r.notFound(w, req)
			return
		}
		http.NotFound(w, req)
		return
	}
	meta := requestMetaFrom(req.Context())
	if meta != nil {
		meta.route = match.route.pattern
		meta.params = match.params
		if id, ok := match.params["request_id"]; ok {
			meta.requestID = id
		}
	}
	if req.Method == http.MethodHead {
		w = headResponseWriter{ResponseWriter: w}
	}
	match.route.handler(w, req, match.params)
}

// headResponseWriter discards the body of a HEAD response while forwarding
// status and headers.
type headResponseWriter struct {
	http.ResponseWriter
}

func (w headResponseWriter) Write(data []byte) (int, error) { return len(data), nil }

type metaKey struct{}

func withRequestMeta(ctx context.Context, meta *requestMeta) context.Context {
	return context.WithValue(ctx, metaKey{}, meta)
}

func requestMetaFrom(ctx context.Context) *requestMeta {
	meta, _ := ctx.Value(metaKey{}).(*requestMeta)
	return meta
}
