export namespace main {
	
	export class PluginManifest {
	    name: string;
	    version: string;
	    api_version: number;
	    description: string;
	    binary: string;
	    provides: string[];
	    actions: Record<string, any>;
	    tags: string[];
	    icon?: string;
	    ui?: boolean;
	    capabilities?: number[];
	
	    static createFrom(source: any = {}) {
	        return new PluginManifest(source);
	    }
	
	    constructor(source: any = {}) {
	        if ('string' === typeof source) source = JSON.parse(source);
	        this.name = source["name"];
	        this.version = source["version"];
	        this.api_version = source["api_version"];
	        this.description = source["description"];
	        this.binary = source["binary"];
	        this.provides = source["provides"];
	        this.actions = source["actions"];
	        this.tags = source["tags"];
	        this.icon = source["icon"];
	        this.ui = source["ui"];
	        this.capabilities = source["capabilities"];
	    }
	}

}

