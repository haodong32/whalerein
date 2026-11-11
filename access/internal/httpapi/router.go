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

func (r *router) match(method, path string) (pathMatch, bool, bool) {
	if path == "/" {
		for i := range r.routes {
			if len(r.routes[i].parts) == 0 {
				if r.routes[i].method == method {
					return pathMatch{route: &r.routes[i], params: map[string]string{}}, true, true
				}
				return pathMatch{route: &r.routes[i]}, true, false
			}
		}
		return pathMatch{}, false, false
	}
	if !strings.HasPrefix(path, "/") || strings.HasSuffix(path, "/") {
		return pathMatch{}, false, false
	}
	segments := strings.Split(strings.TrimPrefix(path, "/"), "/")
	for i := range r.routes {
		spec := &r.routes[i]
		if len(spec.parts) != len(segments) {
			continue
		}
		params := make(map[string]string)
		matched := true
		for index, part := range spec.parts {
			value := segments[index]
			if value == "" {
				matched = false
				break
			}
			if part.param != "" {
				params[part.param] = value
				continue
			}
			if part.literal != value {
				matched = false
				break
			}
		}
		if !matched {
			continue
		}
		if spec.method == method {
			return pathMatch{route: spec, params: params}, true, true
		}
		return pathMatch{route: spec, params: params}, true, false
	}
	return pathMatch{}, false, false
}

func (r *router) ServeHTTP(w http.ResponseWriter, req *http.Request) {
	match, pathMatched, methodMatched := r.match(req.Method, req.URL.Path)
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
		if id, ok := match.params["request_id"]; ok {
			meta.requestID = id
		}
	}
	match.route.handler(w, req, match.params)
}

type metaKey struct{}

func withRequestMeta(ctx context.Context, meta *requestMeta) context.Context {
	return context.WithValue(ctx, metaKey{}, meta)
}

func requestMetaFrom(ctx context.Context) *requestMeta {
	meta, _ := ctx.Value(metaKey{}).(*requestMeta)
	return meta
}
